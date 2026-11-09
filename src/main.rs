mod cache;
mod config;
mod diagram;
mod diff;
mod document;
mod format;
mod formats;
mod highlight;
mod instance;
mod math;
mod picker;
mod reader;
mod render;
mod state;
mod theme;
mod tree;
mod watch;
mod workspace;

use std::io::IsTerminal as _;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::time::Instant;

use futures::StreamExt as _;
use gpui_kit::*;

use instance::Ask;

/// Set on the copy of Rature that runs the window, so it does not leave the terminal again.
const FOREGROUND: &str = "RATURE_FOREGROUND";

const USAGE: &str = "usage: rature [file or folder]

Reads Markdown (and JSON, YAML, CSV, images, code) and follows its changes.
`rt` is the same. Keys: see the README, or `f` `s` `o` `t` `/` `]c` `m` `e` `q` in the window.";

fn main() {
    let started = Instant::now();
    let arg = std::env::args_os().nth(1);
    let ask = match arg.as_deref().and_then(|a| a.to_str()) {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return;
        }
        // From Omarchy's theme-set.d hook: nothing to do when no window is open.
        Some("--theme-changed") => {
            instance::hand_over(&instance::socket(), &Ask::Theme);
            return;
        }
        _ => Ask::Show(arg.map(|arg| {
            let path = PathBuf::from(&arg);
            std::fs::canonicalize(&path).unwrap_or_else(|error| {
                eprintln!("rature: {}: {error}", path.display());
                std::process::exit(1);
            })
        })),
    };
    let socket = instance::socket();
    if instance::hand_over(&socket, &ask) {
        return;
    }
    let Ask::Show(path) = ask else { return };
    let timing = std::env::var_os("RATURE_TIMING").is_some();
    // From a terminal, the window runs on its own and the prompt comes back at once.
    if std::io::stdin().is_terminal() && std::env::var_os(FOREGROUND).is_none() && !timing {
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

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        cx.set_global(config::Config::load());
        cx.set_global(state::State::load());
        cx.set_global(theme::Theme::current());
        reader::bind_keys(cx);
        tree::bind_keys(cx);
        picker::bind_keys(cx);
        workspace::bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(1180.), px(900.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("rature".into()),
            ..Default::default()
        };
        let opened = gpui_kit::open_window(options, cx, |window, cx| {
            if timing {
                window.on_next_frame(move |_, _| {
                    eprintln!("rature: first frame at {:?}", started.elapsed());
                });
            }
            let workspace = cx.new(|cx| workspace::Workspace::new(window, cx));
            workspace.update(cx, |workspace, cx| workspace.open(path, window, cx));
            let reader = workspace.read(cx).reader().clone();
            window.on_window_should_close(cx, move |_, cx| {
                reader.update(cx, |reader, cx| reader.keep_position(cx));
                cx.global::<state::State>().save();
                cx.quit();
                true
            });
            workspace
        });
        let (window, workspace) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("rature: the window cannot open: {error}");
                cx.quit();
                return;
            }
        };
        let (tell, mut asked) = futures::channel::mpsc::unbounded();
        if let Err(error) = instance::serve(&socket, move |ask| {
            let _ = tell.unbounded_send(ask);
        }) {
            eprintln!("rature: other windows will open beside this one: {error}");
        }
        cx.spawn(async move |cx| {
            while let Some(ask) = asked.next().await {
                let _ = window.update(cx, |_, window, cx| match ask {
                    Ask::Show(path) => {
                        if path.is_some() {
                            workspace.update(cx, |workspace, cx| workspace.open(path, window, cx));
                        }
                        window.activate_window();
                    }
                    Ask::Theme => {
                        cx.set_global(theme::Theme::current());
                        window.refresh();
                    }
                });
            }
        })
        .detach();
    });
}
