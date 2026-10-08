//! Hop tray app: a menu bar icon whose menu lists the monitor's ports,
//! checks the active one, and switches on click or on a global hotkey.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use hop_core::caps;
use hop_core::config::{self, Config};
use hop_core::hotkeys::{self, Binding};
use hop_core::menu::{MenuEntry, port_entries};
use hop_core::monitor::Port;
use hop_core::{DdcBackend, Monitor, list_monitors};
use serde::Serialize;
use tauri::image::Image;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

const TRAY_ID: &str = "hop";
const QUIT_ID: &str = "quit";
const SETTINGS_ID: &str = "settings";
const OPEN_AT_LOGIN_ID: &str = "open-at-login";
const PORT_PREFIX: &str = "port:";
/// Fallback refresh, for input changes made outside Hop.
const REFRESH_EVERY: Duration = Duration::from_secs(15);
const MIN_REFRESH_GAP: Duration = Duration::from_secs(1);
/// The monitor sends null replies for about 3 s after a switch.
const QUIET_AFTER_SWITCH: Duration = Duration::from_secs(4);

/// Work for the DDC thread, which owns the backend (it is not `Send`).
enum Job {
    Refresh,
    Switch(u8),
    /// Hotkeys that could not be registered, to show in the menu.
    HotkeyProblems(Vec<String>),
    GetSettings(Sender<Result<Settings, String>>),
    /// Saves one port's settings to the config file.
    Edit {
        code: u8,
        change: PortChange,
        reply: Sender<Result<(), String>>,
    },
    /// Stops the hotkeys while the Settings window records a new one, so
    /// pressing a current hotkey does not switch the monitor.
    PauseHotkeys,
    /// Registers the hotkeys again after a recording ends or is cancelled.
    ResumeHotkeys,
}

enum PortChange {
    Label {
        label: String,
        hidden: bool,
    },
    Hotkey(Option<String>),
    /// A port the capabilities string leaves out.
    Add,
    /// Only for ports added by hand.
    Remove,
}

impl Job {
    /// Answers a waiting Settings window when the job cannot run.
    fn fail(self, error: &str) {
        match self {
            Job::GetSettings(reply) => {
                let _ = reply.send(Err(error.to_string()));
            }
            Job::Edit { reply, .. } => {
                let _ = reply.send(Err(error.to_string()));
            }
            _ => {}
        }
    }
}

#[derive(Serialize)]
struct Settings {
    monitor: String,
    ports: Vec<PortView>,
    /// (code, name) pairs for the Add port picker.
    standard_inputs: Vec<(u8, &'static str)>,
}

#[derive(Serialize)]
struct PortView {
    code: u8,
    name: String,
    label: Option<String>,
    hidden: bool,
    hotkey: Option<String>,
    detected: bool,
}

/// Sends a job to the DDC worker, which owns the monitor state, and waits
/// for its reply.
fn ask<T>(
    jobs: &Sender<Job>,
    job: impl FnOnce(Sender<Result<T, String>>) -> Job,
) -> Result<T, String> {
    let (reply, answer) = mpsc::channel();
    jobs.send(job(reply)).map_err(|e| e.to_string())?;
    answer.recv().map_err(|e| e.to_string())?
}

// `async` runs these off the main thread; the worker can take a second.
#[tauri::command(async)]
fn get_settings(jobs: State<'_, Sender<Job>>) -> Result<Settings, String> {
    ask(&jobs, Job::GetSettings)
}

#[tauri::command(async)]
fn save_port(
    jobs: State<'_, Sender<Job>>,
    code: u8,
    label: String,
    hidden: bool,
) -> Result<(), String> {
    let change = PortChange::Label { label, hidden };
    ask(&jobs, |reply| Job::Edit {
        code,
        change,
        reply,
    })
}

/// Sets a port's hotkey, or clears it when `hotkey` is null.
#[tauri::command(async)]
fn set_hotkey(
    jobs: State<'_, Sender<Job>>,
    code: u8,
    hotkey: Option<String>,
) -> Result<(), String> {
    if let Some(hotkey) = &hotkey {
        hotkey
            .parse::<tauri_plugin_global_shortcut::Shortcut>()
            .map_err(|e| format!("{hotkey} cannot be used: {e}"))?;
    }
    let change = PortChange::Hotkey(hotkey);
    ask(&jobs, |reply| Job::Edit {
        code,
        change,
        reply,
    })
}

/// Adds a port by code, written in decimal ("17") or hex ("0x11").
#[tauri::command(async)]
/// Returns the added port's name, so the window can show which input a
/// typed code means (a bare "11" is decimal: Component 3, not HDMI 1).
fn add_port(jobs: State<'_, Sender<Job>>, code: String) -> Result<String, String> {
    let code = caps::parse_code(&code)
        .ok_or_else(|| format!("\"{code}\" is not an input code: use 1–255 or 0x01–0xFF"))?;
    ask(&jobs, |reply| Job::Edit {
        code,
        change: PortChange::Add,
        reply,
    })?;
    Ok(format!("{} ({code}, 0x{code:02X})", caps::port_name(code)))
}

#[tauri::command(async)]
fn remove_port(jobs: State<'_, Sender<Job>>, code: u8) -> Result<(), String> {
    ask(&jobs, |reply| Job::Edit {
        code,
        change: PortChange::Remove,
        reply,
    })
}

#[tauri::command]
fn pause_hotkeys(jobs: State<'_, Sender<Job>>) {
    let _ = jobs.send(Job::PauseHotkeys);
}

#[tauri::command]
fn resume_hotkeys(jobs: State<'_, Sender<Job>>) {
    let _ = jobs.send(Job::ResumeHotkeys);
}

/// What the menu shows.
#[derive(Clone, PartialEq)]
enum MenuState {
    Loading,
    /// Port entries, then warnings such as hotkeys that failed to register.
    Ready(Vec<MenuEntry>, Vec<String>),
    Failed(String),
}

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .on_window_event(|window, event| {
            // A recording may still be running when Settings closes; make
            // sure the hotkeys come back.
            if window.label() == SETTINGS_ID && matches!(event, WindowEvent::Destroyed) {
                let _ = window.state::<Sender<Job>>().send(Job::ResumeHotkeys);
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_port,
            set_hotkey,
            add_port,
            remove_port,
            pause_hotkeys,
            resume_hotkeys
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            open_at_login_once(app.handle());

            let (jobs, rx) = mpsc::channel();
            TrayIconBuilder::with_id(TRAY_ID)
                .icon(Image::from_bytes(include_bytes!("../icons/tray.png"))?)
                .icon_as_template(true)
                .tooltip("Hop")
                .menu(&build_menu(app.handle(), &MenuState::Loading)?)
                .on_menu_event({
                    let jobs = jobs.clone();
                    move |app, event| on_menu_click(app, event.id().as_ref(), &jobs)
                })
                .on_tray_icon_event({
                    let jobs = jobs.clone();
                    // Hovering comes before the click that opens the menu, so the
                    // checkmark is fresh when the menu shows.
                    move |_, event| {
                        if let TrayIconEvent::Enter { .. } = event {
                            let _ = jobs.send(Job::Refresh);
                        }
                    }
                })
                .build(app)?;

            app.manage(jobs.clone());
            let handle = app.handle().clone();
            let worker_jobs = jobs.clone();
            thread::spawn(move || ddc_worker(&handle, rx, worker_jobs));
            thread::spawn(move || {
                loop {
                    thread::sleep(REFRESH_EVERY);
                    if jobs.send(Job::Refresh).is_err() {
                        break;
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build Hop");

    app.run(|_, event| {
        // A tray app has no windows; stay alive until Quit.
        if let RunEvent::ExitRequested {
            api, code: None, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}

fn on_menu_click(app: &AppHandle, id: &str, jobs: &Sender<Job>) {
    if id == QUIT_ID {
        app.exit(0);
    } else if id == SETTINGS_ID {
        open_settings(app);
    } else if id == OPEN_AT_LOGIN_ID {
        toggle_open_at_login(app);
    } else if let Some(code) = id.strip_prefix(PORT_PREFIX).and_then(|c| c.parse().ok()) {
        let _ = jobs.send(Job::Switch(code));
    }
}

/// Owns the DDC backend. Reads ports once (the capabilities read takes about
/// 1 s), then only re-reads the active input.
fn ddc_worker(app: &AppHandle, jobs: Receiver<Job>, sender: Sender<Job>) {
    let backend = hop_core::default_backend();
    let mut loaded: Option<(Monitor, Config)> = None;
    let mut shown = MenuState::Loading;
    let mut warnings: Vec<String> = Vec::new();
    // True while the Settings window records a hotkey.
    let mut paused = false;
    let mut last_refresh: Option<Instant> = None;
    // No reads until the monitor's null replies after a switch are over.
    let mut quiet_until = Instant::now();
    let mut next = Some(Job::Refresh);

    loop {
        let job = match next.take().map_or_else(|| jobs.recv(), Ok) {
            Ok(job) => job,
            Err(_) => return,
        };
        // Not loaded yet, e.g. the monitor was asleep at login: retry.
        let just_loaded = loaded.is_none();
        if just_loaded {
            match load_monitor(&backend) {
                Ok((m, config)) => {
                    warnings = bind_hotkeys(app, &m, &config, &sender);
                    loaded = Some((m, config));
                }
                Err(e) => {
                    job.fail(&e);
                    let state = MenuState::Failed(e);
                    if state != shown {
                        show(app, &state);
                        shown = state;
                    }
                    continue;
                }
            }
        }
        let (m, config) = loaded.as_mut().expect("loaded above");
        match job {
            // A fresh load already read the active input.
            Job::Refresh if just_loaded => {}
            Job::Refresh => {
                let busy = Instant::now() < quiet_until;
                let recent = last_refresh.is_some_and(|t| t.elapsed() < MIN_REFRESH_GAP);
                if busy || recent {
                    continue;
                }
                // Keep the last known input when the read fails.
                if let Ok(code) = backend.get_input(&m.display) {
                    m.active = Ok(code);
                }
                last_refresh = Some(Instant::now());
            }
            Job::Switch(code) => match backend.set_input(&m.display, code) {
                // Show the request now; a later refresh confirms it.
                Ok(()) => {
                    m.active = Ok(code);
                    quiet_until = Instant::now() + QUIET_AFTER_SWITCH;
                }
                Err(e) => eprintln!("hop: could not switch to {code}: {e}"),
            },
            Job::HotkeyProblems(problems) => {
                for problem in problems {
                    if !warnings.contains(&problem) {
                        warnings.push(problem);
                    }
                }
            }
            Job::GetSettings(reply) => {
                let _ = reply.send(Ok(settings_view(m, config)));
            }
            Job::Edit {
                code,
                change,
                reply,
            } => {
                let result = apply_edit(m, config, code, change);
                if result.is_ok() && !paused {
                    // The file may have gained this monitor's entry or new
                    // hotkeys; bind them again, with fresh warnings.
                    warnings = bind_hotkeys(app, m, config, &sender);
                }
                let _ = reply.send(result);
            }
            Job::PauseHotkeys => {
                paused = true;
                register_hotkeys(app, Vec::new(), sender.clone());
            }
            Job::ResumeHotkeys => {
                paused = false;
                warnings = bind_hotkeys(app, m, config, &sender);
            }
        }
        let state = MenuState::Ready(port_entries(m), warnings.clone());
        // Replacing the menu can close it while it is open; only do it on a change.
        if state != shown {
            show(app, &state);
            shown = state;
        }
    }
}

/// Registers the monitor's hotkeys; returns warnings to show in the menu.
fn bind_hotkeys(
    app: &AppHandle,
    m: &Monitor,
    config: &Config,
    sender: &Sender<Job>,
) -> Vec<String> {
    let settings = config.monitor(&m.display);
    let bound = hotkeys::bindings(settings);
    let mut warnings = bound.conflicts;
    if settings.is_none() {
        warnings.push(format!(
            "No hotkeys: the config has no entry for {}",
            m.display.name
        ));
    }
    register_hotkeys(app, bound.bindings, sender.clone());
    warnings
}

/// The Settings window's view of the monitor: every port, hidden or not.
fn settings_view(m: &Monitor, config: &Config) -> Settings {
    let settings = config.monitor(&m.display);
    let hotkey = |code| {
        settings
            .and_then(|s| s.ports.iter().find(|p| p.code == code))
            .and_then(|p| p.hotkey.clone())
    };
    Settings {
        monitor: m.display.name.clone(),
        ports: m
            .ports
            .iter()
            .map(|p| PortView {
                code: p.code,
                name: p.name.clone(),
                label: p.label.clone(),
                hidden: p.hidden,
                hotkey: hotkey(p.code),
                detected: p.detected,
            })
            .collect(),
        standard_inputs: caps::STANDARD_INPUTS.to_vec(),
    }
}

/// Saves one port's settings to the config file, then applies them to the
/// live monitor so the menu updates at once. Re-reads the file first, so
/// edits made by hand while Hop runs are kept.
fn apply_edit(
    m: &mut Monitor,
    config: &mut Config,
    code: u8,
    change: PortChange,
) -> Result<(), String> {
    let path = config::default_path();
    let mut updated = config::load_or_create(&path, std::slice::from_ref(&m.display))
        .map_err(|e| e.to_string())?;
    let existing = m.ports.iter().find(|p| p.code == code);
    match &change {
        PortChange::Label { label, hidden } => updated.set_port(&m.display, code, label, *hidden),
        PortChange::Hotkey(hotkey) => updated
            .set_hotkey(&m.display, code, hotkey.as_deref())
            .map_err(|e| e.to_string())?,
        PortChange::Add => match existing {
            Some(port) => {
                return Err(format!(
                    "Input {code} is already listed as {}",
                    port.title()
                ));
            }
            None => updated.add_port(&m.display, code),
        },
        PortChange::Remove => match existing {
            Some(port) if port.detected => {
                return Err(format!(
                    "{} is reported by the monitor; hide it instead",
                    port.title()
                ));
            }
            _ => updated.remove_port(&m.display, code),
        },
    }
    updated.save(&path).map_err(|e| e.to_string())?;
    *config = updated;
    match change {
        PortChange::Add => m.ports.push(Port {
            code,
            name: caps::port_name(code),
            label: None,
            hidden: false,
            detected: false,
        }),
        PortChange::Remove => m.ports.retain(|p| p.code != code),
        _ => {}
    }
    let saved = config
        .monitor(&m.display)
        .and_then(|c| c.ports.iter().find(|p| p.code == code));
    if let (Some(port), Some(saved)) = (m.ports.iter_mut().find(|p| p.code == code), saved) {
        port.label = saved.label.clone();
        port.hidden = saved.hidden;
    }
    Ok(())
}

fn load_monitor(backend: &dyn DdcBackend) -> Result<(Monitor, Config), String> {
    let config = config::load_for(backend).map_err(|e| e.to_string())?;
    let monitor = list_monitors(backend, &config)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "No DDC/CI monitor found".to_string())?;
    Ok((monitor, config))
}

/// Registers each binding on the main thread. Carbon hotkeys on macOS need
/// no Accessibility or Input Monitoring permission. Failures go back to the
/// worker, which shows them in the menu.
fn register_hotkeys(app: &AppHandle, bindings: Vec<Binding>, jobs: Sender<Job>) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let shortcuts = handle.global_shortcut();
        let _ = shortcuts.unregister_all();
        let mut problems = Vec::new();
        for Binding { hotkey, code } in bindings {
            let on_press = {
                let jobs = jobs.clone();
                move |_: &AppHandle, _: &_, event: tauri_plugin_global_shortcut::ShortcutEvent| {
                    if event.state() == ShortcutState::Pressed {
                        let _ = jobs.send(Job::Switch(code));
                    }
                }
            };
            if let Err(e) = shortcuts.on_shortcut(hotkey.as_str(), on_press) {
                let problem = format!("Hotkey {hotkey} is not active: {e}");
                eprintln!("hop: {problem}");
                problems.push(problem);
            }
        }
        if !problems.is_empty() {
            let _ = jobs.send(Job::HotkeyProblems(problems));
        }
    });
}

/// Turns on Open at Login the first time the installed app runs. A build run
/// from the source tree is left alone, because the login item records the
/// app's path. A marker file keeps a later "off" choice.
fn open_at_login_once(app: &AppHandle) {
    let installed = std::env::current_exe().is_ok_and(|exe| exe.starts_with("/Applications"));
    let marker = config::default_path().with_file_name("login-item-set");
    if !installed || marker.exists() {
        return;
    }
    if let Err(e) = app.autolaunch().enable() {
        eprintln!("hop: could not turn on Open at Login: {e}");
        return;
    }
    let written = marker
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&marker, ""));
    if let Err(e) = written {
        eprintln!("hop: could not record the login item choice: {e}");
    }
}

fn toggle_open_at_login(app: &AppHandle) {
    let launcher = app.autolaunch();
    let result = if launcher.is_enabled().unwrap_or(false) {
        launcher.disable()
    } else {
        launcher.enable()
    };
    if let Err(e) = result {
        eprintln!("hop: could not change Open at Login: {e}");
    }
    // The check item flips itself on click; later menu rebuilds read the
    // real state from the login item.
}

/// Shows the Settings window, creating it on first use.
fn open_settings(app: &AppHandle) {
    let window = match app.get_webview_window(SETTINGS_ID) {
        Some(window) => window,
        None => {
            match WebviewWindowBuilder::new(app, SETTINGS_ID, WebviewUrl::App("index.html".into()))
                .title("Hop Settings")
                .inner_size(640.0, 380.0)
                .resizable(false)
                .build()
            {
                Ok(window) => window,
                Err(e) => {
                    eprintln!("hop: could not open Settings: {e}");
                    return;
                }
            }
        }
    };
    let _ = window.show();
    let _ = window.set_focus();
}

/// Rebuilds the tray menu on the main thread.
fn show(app: &AppHandle, state: &MenuState) {
    let handle = app.clone();
    let state = state.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(tray) = handle.tray_by_id(TRAY_ID) else {
            return;
        };
        match build_menu(&handle, &state) {
            Ok(menu) => {
                let _ = tray.set_menu(Some(menu));
            }
            Err(e) => eprintln!("hop: could not build the menu: {e}"),
        }
    });
}

fn build_menu(app: &AppHandle, state: &MenuState) -> tauri::Result<Menu<Wry>> {
    let mut menu = MenuBuilder::new(app);
    match state {
        MenuState::Loading => {
            menu = menu.item(
                &MenuItemBuilder::new("Reading monitor…")
                    .enabled(false)
                    .build(app)?,
            );
        }
        MenuState::Failed(e) => {
            menu = menu.item(&MenuItemBuilder::new(e).enabled(false).build(app)?);
        }
        MenuState::Ready(entries, warnings) => {
            for warning in warnings {
                menu = menu.item(
                    &MenuItemBuilder::new(format!("⚠ {warning}"))
                        .enabled(false)
                        .build(app)?,
                );
            }
            if !warnings.is_empty() {
                menu = menu.separator();
            }
            for entry in entries {
                let item = CheckMenuItemBuilder::with_id(
                    format!("{PORT_PREFIX}{}", entry.code),
                    &entry.title,
                )
                .checked(entry.checked)
                .build(app)?;
                menu = menu.item(&item);
            }
        }
    }
    menu.separator()
        .item(&MenuItemBuilder::with_id(SETTINGS_ID, "Settings…").build(app)?)
        .item(
            &CheckMenuItemBuilder::with_id(OPEN_AT_LOGIN_ID, "Open at Login")
                .checked(app.autolaunch().is_enabled().unwrap_or(false))
                .build(app)?,
        )
        .item(&MenuItemBuilder::with_id(QUIT_ID, "Quit Hop").build(app)?)
        .build()
}
