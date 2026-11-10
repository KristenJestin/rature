mod cache;
mod config;
mod diagram;
mod diff;
mod document;
mod format;
mod formats;
mod fsio;
mod highlight;
mod icons;
mod instance;
mod kit;
mod list;
mod live;
mod load;
mod math;
mod picker;
mod reader;
mod render;
mod start;
mod state;
mod table;
mod timing;
mod tree;
mod watch;
mod workspace;

use std::io::IsTerminal as _;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::time::Instant;

use futures::StreamExt as _;
use gpui_kit::*;

/// Set on the copy of Rature that runs the window, so it does not leave the terminal again.
const FOREGROUND: &str = "RATURE_FOREGROUND";

const USAGE: &str = "usage: rature [file or folder]

Reads Markdown (and JSON, YAML, CSV, images, code) and follows its changes.
`rt` is the same. Keys: see the README, or `f` `s` `o` `t` `Ctrl-f` `]c` `m` `i` `e` `q` in the
window.";

fn main() {
    let started = Instant::now();
    let timed = std::env::var_os("RATURE_TIMING").is_some();
    if timed {
        timing::start(started);
        timing::mark("main entered");
    }
    // Read once, then taken out of the environment, so that what Rature starts (nvim in a
    // terminal, and what that starts) does not inherit it.
    let foreground = std::env::var_os(FOREGROUND).is_some();
    if foreground {
        // SAFETY: edition 2024 makes this unsafe because the environment is process-global and
        // not synchronised. This runs at the top of `main`, before any thread is started (GPUI's,
        // the watcher's, the socket's), so nothing can read the environment concurrently.
        unsafe { std::env::remove_var(FOREGROUND) };
    }
    let arg = std::env::args_os().nth(1);
    let path = match arg.as_deref().and_then(|a| a.to_str()) {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return;
        }
        _ => arg.map(|arg| {
            let path = PathBuf::from(&arg);
            std::fs::canonicalize(&path).unwrap_or_else(|error| {
                eprintln!("rature: {}: {error}", path.display());
                std::process::exit(1);
            })
        }),
    };
    let socket = instance::socket();
    if instance::hand_over(&socket, path.as_deref()) {
        return;
    }
    timing::mark("no other Rature to hand over to");
    // From a terminal, the window runs on its own and the prompt comes back at once.
    if std::io::stdin().is_terminal() && !foreground && !timed {
        let spawned = std::process::Command::new(
            std::env::current_exe().expect("a running binary has a path"),
        )
        .args(&path)
        .env(FOREGROUND, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
        if let Err(error) = spawned {
            eprintln!("rature: cannot start the window: {error}");
            std::process::exit(1);
        }
        return;
    }

    // Read while GPUI starts its platform, which takes far longer: the window needs it only then.
    let state = std::thread::Builder::new()
        .name("rature-state-load".into())
        .spawn(state::State::load);
    let application = gpui_kit::application().with_assets(icons::Assets);
    timing::mark("application created (platform up)");
    application.run(move |cx| {
        timing::mark("application running");
        let desktop = timing::time("Omarchy theme read", || {
            gpui_design_system::omarchy::Omarchy::from_system()
        });
        timing::time("design system init (kit, fonts, theme)", || {
            gpui_design_system::init(
                gpui_design_system::Identity::new("Rature").desktop(desktop),
                cx,
            )
        });
        cx.set_global(timing::time("config loaded", config::Config::load));
        cx.set_global(timing::time("state loaded", || match state {
            Ok(loading) => loading
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(_) => state::State::load(),
        }));
        cx.background_executor()
            .spawn(async { timing::time("syntect warmed (off the main thread)", highlight::warm) })
            .detach();
        timing::time("keys bound", || {
            reader::bind_keys(cx);
            live::bind_keys(cx);
            list::bind_keys(cx);
            table::bind_keys(cx);
            tree::bind_keys(cx);
            picker::bind_keys(cx);
            start::bind_keys(cx);
            workspace::bind_keys(cx);
        });
        let bounds = Bounds::centered(None, size(px(1180.), px(900.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("rature".into()),
            ..Default::default()
        };
        timing::mark("open_window called");
        let opened = gpui_kit::open_window(options, cx, |window, cx| {
            timing::mark("window created");
            if timed {
                window.on_next_frame(move |window, _| {
                    timing::mark("first frame requested");
                    eprintln!("rature: first frame at {:?}", started.elapsed());
                    // Asked for once the compositor has shown the first frame.
                    window.on_next_frame(|_, _| {
                        timing::mark("second frame requested (first one shown)");
                        timing::end();
                    });
                });
            }
            let workspace = timing::time("workspace made", || {
                cx.new(|cx| workspace::Workspace::new(window, cx))
            });
            timing::time("workspace opened its path", || {
                workspace.update(cx, |workspace, cx| workspace.open(path, window, cx))
            });
            let reader = workspace.read(cx).reader().clone();
            // The file asked for is read off the main thread: the timings wait for it too.
            timing::expect(1 + usize::from(reader.read(cx).path().is_some()));
            window.on_window_should_close(cx, move |_, cx| {
                reader.update(cx, |reader, cx| reader.put_down(cx));
                cx.global::<state::State>().save();
                state::flush();
                cx.quit();
                true
            });
            workspace
        });
        timing::mark("open_window returned (its first draw done)");
        let (window, workspace) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("rature: the window cannot open: {error}");
                cx.quit();
                return;
            }
        };
        let (tell, mut asked) = futures::channel::mpsc::unbounded();
        if let Err(error) = instance::serve(&socket, move |path| {
            let _ = tell.unbounded_send(path);
        }) {
            eprintln!("rature: other windows will open beside this one: {error}");
        }
        cx.spawn(async move |cx| {
            while let Some(path) = asked.next().await {
                let _ = window.update(cx, |_, window, cx| {
                    if path.is_some() {
                        workspace.update(cx, |workspace, cx| workspace.open(path, window, cx));
                    }
                    window.activate_window();
                });
            }
        })
        .detach();
    });
}
