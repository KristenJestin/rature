mod diagram;
mod document;
mod instance;
mod math;
mod theme;
mod view;
mod watch;

use std::io::IsTerminal as _;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::time::Instant;

use futures::StreamExt as _;
use gpui_kit::*;

const WELCOME: &str =
    "# Rature\n\nOpen a file from a terminal: `rature file.md`, or `rt file.md`.\n";

/// Set on the copy of Rature that runs the window, so it does not leave the terminal again.
const FOREGROUND: &str = "RATURE_FOREGROUND";

fn main() {
    let started = Instant::now();
    let path = std::env::args_os().nth(1).map(|arg| {
        let path = PathBuf::from(&arg);
        std::fs::canonicalize(&path).unwrap_or_else(|error| {
            eprintln!("rature: {}: {error}", path.display());
            std::process::exit(1);
        })
    });
    let socket = instance::socket();
    if instance::hand_over(&socket, path.as_deref()) {
        return;
    }
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
        view::bind_keys(cx);
        let bounds = Bounds::centered(None, size(px(960.), px(900.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("rature".into()),
            ..Default::default()
        };
        let opened = gpui_kit::open_window(options, cx, |window, cx| {
            window.on_window_should_close(cx, |_, cx| {
                cx.quit();
                true
            });
            if timing {
                window.on_next_frame(move |_, _| {
                    eprintln!("rature: first frame at {:?}", started.elapsed());
                });
            }
            let reader = cx.new(|cx| {
                let mut reader = view::Reader::new(cx);
                match path {
                    Some(path) => reader.open(path, cx),
                    None => reader.show(WELCOME, cx),
                }
                reader
            });
            let focus = reader.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
            reader
        });
        let (window, reader) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("rature: the window cannot open: {error}");
                cx.quit();
                return;
            }
        };
        let (ask, mut asked) = futures::channel::mpsc::unbounded();
        if let Err(error) = instance::serve(&socket, move |path| {
            let _ = ask.unbounded_send(path);
        }) {
            eprintln!("rature: other windows will open beside this one: {error}");
        }
        cx.spawn(async move |cx| {
            while let Some(path) = asked.next().await {
                let _ = window.update(cx, |_, window, cx| {
                    if let Some(path) = path {
                        reader.update(cx, |reader, cx| reader.open(path, cx));
                    }
                    window.activate_window();
                });
            }
        })
        .detach();
    });
}
