// GUI subsystem on Windows (issue #40, specs/01-architecture.md): without
// this, a double-clicked fastcull-app.exe drags a console window along, and
// closing that console kills the app (CTRL_CLOSE_EVENT). No-op on Linux.
// fastcull-cli deliberately stays a console-subsystem terminal tool.
#![windows_subsystem = "windows"]

//! FastCull desktop application: thin Slint bridge over `fastcull-core`
//! (specs/modules/ui-grid.md). All layout math lives in `fastcull_core::grid`;
//! this crate only moves data between the engine and the declarative UI.
//!
//! Usage: `fastcull-app [<folder>]` or `fastcull-app --synthetic 2000` —
//! no arguments opens the empty window (desktop-launcher start, issue #5)
//! (colored placeholder cells, no RAW files needed — the M2 60 fps spike).
//!
//! Layout: this file parses the command line, builds the window and the one
//! [`AppState`], and wires the controllers. One controller per surface or
//! concern, and each one names itself:
//!
//! | file | when you open it |
//! |---|---|
//! | `state.rs` | the state every controller borrows, and the app constants |
//! | `session.rs` | opening a folder, launch dispatch, templates, ui.toml prefs |
//! | `nav.rs` | keyboard navigation, marks, filter/sort, cursor reveal |
//! | `loupe_ctrl.rs` | pointer gestures, loupe geometry, the full-res ring |
//! | `presenter.rs` | the refresh pass: state -> window properties, cells, and which loupe rung is shown |
//! | `pump.rs` | the 33 ms engine tick and texture adoption |
//! | `iptc_bridge.rs` | the IPTC panel |
//! | `copy_bridge.rs` | the Copy Picks dialog (and burst regrouping) |
//! | `focus.rs` | keyboard focus continuity |
//! | `shutter.rs` | `--screenshot` readiness gate and shutdown |
//! | `harness.rs` | the FASTCULL_DRIVE scripted-action interpreter |
//! | `trace.rs` | the FASTCULL_TRACE log |
//! | `kitchen.rs` | the texture worker (pixels -> textures, off the UI thread) |
//!
//! Controllers call `fastcull-core` and funnel their UI updates through
//! [`presenter::refresh`]. They DO call each other: a helper two surfaces
//! need lives in the module that owns the concern, and the other module
//! imports it — `nav` takes the loupe's factor math from `loupe_ctrl`,
//! `session`/`copy_bridge`/`iptc_bridge` take `refocus_topmost_deferred`
//! from `focus`, `presenter` takes `refresh_iptc_panel` from `iptc_bridge`.
//! The split moved code, not calls: this is the pre-split call graph
//! unchanged, and the `use crate::` block at the top of each module is that
//! module's outgoing edge list — read it first when tracing a behavior
//! across surfaces.

use std::cell::RefCell;
use std::rc::Rc;

mod copy_bridge;
mod focus;
mod harness;
mod iptc_bridge;
mod kitchen;
mod loupe_ctrl;
mod nav;
mod presenter;
mod pump;
mod session;
mod shutter;
mod state;
mod trace;

use crate::presenter::refresh;
use crate::session::Launch;
use crate::state::{clamp_wash_opacity, AppState, SELECTION_WASH_OPACITY, SELECTION_WASH_RGB};
use crate::trace::trace_mark;
use fastcull_core::grid;
use slint::{ComponentHandle, VecModel};

slint::include_modules!();

/// Reconnect stderr/stdout to the parent's console (issue #40). A GUI-
/// subsystem process launched from cmd/PowerShell WITHOUT redirection starts
/// with NULL std handles, so every `eprintln!` — the FASTCULL_TRACE marks the
/// FAQ tells bug reporters to capture, usage errors, the drive harness —
/// would silently vanish. Attaching to the parent's console makes Windows
/// replace NULL std handles with console handles (GetStdHandle docs,
/// "Attach/detach behavior"), and Rust's std re-queries the handle on every
/// write rather than caching it, so no further rebinding is needed.
/// Redirected handles (`2> trace.txt`, test pipes) arrive via
/// STARTF_USESTDHANDLES and are never replaced. Failure (no parent console —
/// the Explorer double-click) is the normal GUI launch: ignore it.
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: no pointers, no preconditions; the call either attaches the
    // process to its parent's console or fails harmlessly.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn main() {
    // Must run before ANY output so the first trace/usage line already has a
    // console to land on.
    #[cfg(windows)]
    attach_parent_console();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // --screenshot <out.png>: render, snapshot after a settle delay, save,
    // exit 0. The screenshot smoke-test hook (ui-grid.md acceptance).
    let screenshot: Option<std::path::PathBuf> = match args.iter().position(|a| a == "--screenshot")
    {
        Some(i) if i + 1 < args.len() => {
            let path = args.remove(i + 1);
            args.remove(i);
            Some(path.into())
        }
        Some(_) => {
            eprintln!("usage: --screenshot <out.jpg>");
            std::process::exit(2);
        }
        None => None,
    };
    if screenshot.is_some() {
        // take_snapshot() yields black frames on the GPU renderer; the
        // software renderer supports it and is fine for smoke tests.
        std::env::set_var("SLINT_BACKEND", "winit-software");
    }
    // --start-loupe / --start-11: open directly at loupe zoom (fit or 1:1) —
    // used by the screenshot smoke tests to capture those states.
    let start_11 = args
        .iter()
        .position(|a| a == "--start-11")
        .map(|i| args.remove(i))
        .is_some();
    let start_loupe = args
        .iter()
        .position(|a| a == "--start-loupe")
        .map(|i| args.remove(i))
        .is_some();
    let launch = match args.as_slice() {
        [] => Launch::Empty,
        [flag, n] if flag == "--synthetic" => {
            let Ok(n) = n.parse::<usize>() else {
                eprintln!("usage: fastcull-app [<folder> | --synthetic <count>]");
                std::process::exit(2);
            };
            Launch::Synthetic(n)
        }
        [folder] => Launch::Folder(folder.into()),
        _ => {
            eprintln!("usage: fastcull-app [<folder> | --synthetic <count>]");
            std::process::exit(2);
        }
    };

    let window = MainWindow::new().expect("creating window");
    // About-dialog version (issue #23): X.Y.Z on a release-tag build,
    // X.Y.Z-devel-YYYYMMDD-<hash> otherwise (suffix composed by build.rs — a bug
    // report from a dev build must pin the commit). Traced so headless
    // runs can assert the composition without pixel-reading the dialog.
    let about_version = format!(
        "{}{}",
        fastcull_core::VERSION,
        env!("FASTCULL_VERSION_SUFFIX")
    );
    trace_mark(&format!("about version {about_version}"));
    window.set_about_version(about_version.into());
    // Selection wash defaults. The UI only ever READS these two properties,
    // so promoting the strength to a user setting later is a write here.
    window.set_selection_wash(slint::Color::from_rgb_u8(
        SELECTION_WASH_RGB[0],
        SELECTION_WASH_RGB[1],
        SELECTION_WASH_RGB[2],
    ));
    window.set_selection_wash_opacity(clamp_wash_opacity(SELECTION_WASH_OPACITY));
    let cells = Rc::new(VecModel::from(Vec::<CellData>::new()));
    window.set_cells(slint::ModelRc::from(Rc::clone(&cells)));
    let start_at_loupe = start_11 || start_loupe;
    let state = Rc::new(RefCell::new(AppState {
        session: Default::default(),
        // Only the launch zoom differs from the grid's own defaults
        // (--start-loupe/--start-11 open straight at the loupe step).
        grid: state::GridViewState {
            zoom: if start_at_loupe {
                grid::ZOOM_COLUMNS.len() - 1
            } else {
                1 // 8 columns
            },
            ..Default::default()
        },
        textures: Default::default(),
        cells,
        // Only the launch desire differs from the loupe's own defaults
        // (--start-11 pins 1:1 before any texture exists).
        loupe_view: state::LoupeViewState {
            zoom_factor: if start_11 { f32::INFINITY } else { 1.0 },
            ..Default::default()
        },
        kitchen: {
            // Completion nudge: the worker pokes the event loop so a
            // finished texture is adopted as soon as the UI is idle —
            // the 33 ms pump is the fallback, not the design point
            // (persona condition: the one-tick cost must not be a
            // trickle-in).
            let win = window.as_weak();
            kitchen::Kitchen::start(Box::new(move || {
                let win = win.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(win) = win.upgrade() {
                        win.invoke_kitchen_ready();
                    }
                })
                .ok();
            }))
        },
        bursts: Default::default(),
        iptc_panel: Default::default(),
        copy: Default::default(),
    }));

    session::dispatch(&state, launch, start_11);

    // Callback wiring: each controller installs its own surface's callbacks.
    // Registration order does not matter — each `on_…` just stores a closure.
    presenter::wire(&window, &state);
    nav::wire(&window, &state);
    session::wire(&window, &state);
    focus::wire(&window);
    iptc_bridge::wire(&window, &state);
    copy_bridge::wire(&window, &state);
    loupe_ctrl::wire(&window, &state);
    pump::wire(&window, &state);
    window.on_quit(|| {
        slint::quit_event_loop().ok();
    });

    // The pump timer must outlive the event loop: dropping it stops the tick.
    let _timer = pump::start(&window, &state);

    refresh(&window, &state);

    let drives_pending = harness::install(&window, &state);

    let screenshot_requested = screenshot.is_some();
    // The shutter timer must outlive the event loop — dropping it cancels
    // the poll — so main keeps the binding even though it never touches it.
    let (_shot_timer, shot_written) = shutter::arm(&window, &state, screenshot, &drives_pending);

    window.run().expect("running event loop");
    shutter::finish(screenshot_requested, &shot_written);
    shutter::shutdown(&state);
}
