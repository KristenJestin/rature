mod diagram;
mod document;
mod math;
mod theme;
mod view;

use std::time::Instant;

use gpui_kit::*;

fn main() {
    let started = Instant::now();
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: rature <file.md>");
        std::process::exit(2);
    };
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("rature: {path}: {error}");
            std::process::exit(1);
        }
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
