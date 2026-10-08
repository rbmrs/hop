//! Hop tray app: a menu bar icon whose menu lists the monitor's ports,
//! checks the active one, and switches on click or on a global hotkey.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use hop_core::config::{self, Config};
use hop_core::hotkeys::{self, Binding};
use hop_core::menu::{MenuEntry, port_entries};
use hop_core::{DdcBackend, Monitor, list_monitors};
use serde::Serialize;
use tauri::image::Image;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

const TRAY_ID: &str = "hop";
const QUIT_ID: &str = "quit";
const SETTINGS_ID: &str = "settings";
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
    SavePort {
        code: u8,
        label: String,
        hidden: bool,
        reply: Sender<Result<(), String>>,
    },
}

impl Job {
    /// Answers a waiting Settings window when the job cannot run.
    fn fail(self, error: &str) {
        match self {
            Job::GetSettings(reply) => {
                let _ = reply.send(Err(error.to_string()));
            }
            Job::SavePort { reply, .. } => {
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
}

#[derive(Serialize)]
struct PortView {
    code: u8,
    name: String,
    label: Option<String>,
    hidden: bool,
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
    ask(&jobs, |reply| Job::SavePort {
        code,
        label,
        hidden,
        reply,
    })
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
        .invoke_handler(tauri::generate_handler![get_settings, save_port])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

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
            Job::HotkeyProblems(problems) => warnings.extend(problems),
            Job::GetSettings(reply) => {
                let _ = reply.send(Ok(settings_view(m)));
            }
            Job::SavePort {
                code,
                label,
                hidden,
                reply,
            } => {
                let result = apply_port(m, config, code, &label, hidden);
                if result.is_ok() {
                    // The file may have gained this monitor's entry or new
                    // hotkeys; bind them again.
                    warnings = bind_hotkeys(app, m, config, &sender);
                }
                let _ = reply.send(result);
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
fn settings_view(m: &Monitor) -> Settings {
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
            })
            .collect(),
    }
}

/// Saves a port's label and hidden flag to the config file, then applies them
/// to the live monitor so the menu updates at once. Re-reads the file first,
/// so edits made by hand while Hop runs are kept.
fn apply_port(
    m: &mut Monitor,
    config: &mut Config,
    code: u8,
    label: &str,
    hidden: bool,
) -> Result<(), String> {
    let path = config::default_path();
    let mut updated = config::load_or_create(&path, std::slice::from_ref(&m.display))
        .map_err(|e| e.to_string())?;
    updated.set_port(&m.display, code, label, hidden);
    updated.save(&path).map_err(|e| e.to_string())?;
    *config = updated;
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

/// Shows the Settings window, creating it on first use.
fn open_settings(app: &AppHandle) {
    let window = match app.get_webview_window(SETTINGS_ID) {
        Some(window) => window,
        None => {
            match WebviewWindowBuilder::new(app, SETTINGS_ID, WebviewUrl::App("index.html".into()))
                .title("Hop Settings")
                .inner_size(440.0, 300.0)
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
        .item(&MenuItemBuilder::with_id(QUIT_ID, "Quit Hop").build(app)?)
        .build()
}
