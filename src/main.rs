mod diagram;
mod document;
mod math;
mod theme;
mod view;

use std::time::Instant;

use gpui_kit::*;

const WELCOME: &str =
    "# Rature\n\nOpen a file from a terminal: `rature file.md`, or `rt file.md`.\n";

fn main() {
    let started = Instant::now();
    // Started from the launcher, there is no file: the window says how to open one.
    let source = match std::env::args().nth(1) {
        None => WELCOME.to_string(),
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("rature: {path}: {error}");
                std::process::exit(1);
            }
        },
    };
    let blocks = document::parse(&source);
    let parsed = started.elapsed();
    let timing = std::env::var_os("RATURE_TIMING").is_some();

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        let bounds = Bounds::centered(None, size(px(960.), px(900.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some("rature".into()),
            ..Default::default()
        };
        let window = gpui_kit::open_window(options, cx, |window, cx| {
            window.on_window_should_close(cx, |_, cx| {
                cx.quit();
                true
            });
            if timing {
                window.on_next_frame(move |_, _| {
                    eprintln!(
                        "rature: parsed in {parsed:?}, first frame at {:?}",
                        started.elapsed()
                    );
                });
            }
            cx.new(|_| view::Reader::new(blocks))
        });
        if let Err(error) = window {
            eprintln!("rature: the window cannot open: {error}");
            cx.quit();
        }
    });
}
