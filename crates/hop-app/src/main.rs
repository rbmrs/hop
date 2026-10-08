//! Hop tray app: a menu bar icon whose menu lists the monitor's ports,
//! checks the active one, and switches on click.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use hop_core::config;
use hop_core::menu::{MenuEntry, port_entries};
use hop_core::{DdcBackend, Monitor, list_monitors};
use tauri::image::Image;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, RunEvent, Wry};

const TRAY_ID: &str = "hop";
const QUIT_ID: &str = "quit";
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
}

/// What the menu shows.
#[derive(Clone, PartialEq)]
enum MenuState {
    Loading,
    Ready(Vec<MenuEntry>),
    Failed(String),
}

fn main() {
    let app = tauri::Builder::default()
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

            let handle = app.handle().clone();
            thread::spawn(move || ddc_worker(&handle, rx));
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
    } else if let Some(code) = id.strip_prefix(PORT_PREFIX).and_then(|c| c.parse().ok()) {
        let _ = jobs.send(Job::Switch(code));
    }
}

/// Owns the DDC backend. Reads ports once (the capabilities read takes about
/// 1 s), then only re-reads the active input.
fn ddc_worker(app: &AppHandle, jobs: Receiver<Job>) {
    let backend = hop_core::default_backend();
    let mut monitor: Option<Monitor> = None;
    let mut shown = MenuState::Loading;
    let mut last_refresh: Option<Instant> = None;
    // No reads until the monitor's null replies after a switch are over.
    let mut quiet_until = Instant::now();
    let mut next = Some(Job::Refresh);

    loop {
        let job = match next.take().map_or_else(|| jobs.recv(), Ok) {
            Ok(job) => job,
            Err(_) => return,
        };
        let state = match monitor.as_mut() {
            // Not loaded yet, e.g. the monitor was asleep at login: retry.
            None => match load_monitor(&backend) {
                Ok(m) => MenuState::Ready(port_entries(monitor.insert(m))),
                Err(e) => MenuState::Failed(e),
            },
            Some(m) => {
                match job {
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
                }
                MenuState::Ready(port_entries(m))
            }
        };
        // Replacing the menu can close it while it is open; only do it on a change.
        if state != shown {
            show(app, &state);
            shown = state;
        }
    }
}

fn load_monitor(backend: &dyn DdcBackend) -> Result<Monitor, String> {
    let config = config::load_for(backend).map_err(|e| e.to_string())?;
    list_monitors(backend, &config)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "No DDC/CI monitor found".to_string())
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
        MenuState::Ready(entries) => {
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
        .item(&MenuItemBuilder::with_id(QUIT_ID, "Quit Hop").build(app)?)
        .build()
}
