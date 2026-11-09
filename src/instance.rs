//! One window: a second `rature` hands its file to the running one, over a socket in the
//! runtime folder, and exits.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

pub fn socket() -> PathBuf {
    let folder = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    folder.join("rature.sock")
}

/// What a second `rature` asks of the running one.
#[derive(Debug, PartialEq, Eq)]
pub enum Ask {
    /// The window to the front, with this file or folder in it if any.
    Show(Option<PathBuf>),
    /// The Omarchy theme changed: read it again.
    Theme,
}

impl Ask {
    fn line(&self) -> String {
        match self {
            Ask::Show(path) => path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Ask::Theme => "!theme".into(),
        }
    }

    fn from_line(line: &str) -> Self {
        match line {
            "!theme" => Ask::Theme,
            "" => Ask::Show(None),
            path => Ask::Show(Some(PathBuf::from(path))),
        }
    }
}

/// Gives `ask` to the running Rature. False when none runs.
pub fn hand_over(socket: &Path, ask: &Ask) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    writeln!(stream, "{}", ask.line()).is_ok()
}

/// Listens for what other `rature` ask, on a thread of its own.
pub fn serve(socket: &Path, on_ask: impl Fn(Ask) + Send + 'static) -> std::io::Result<()> {
    // Nobody answered `hand_over`: a socket left there is a dead instance's.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_ok() {
                on_ask(Ask::from_line(line.trim_end_matches('\n')));
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_second_rature_hands_its_file_to_the_first() {
        let folder = std::env::temp_dir().join(format!("rature-instance-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let socket = folder.join("rature.sock");
        assert!(!hand_over(&socket, &Ask::Show(None)), "nobody listens yet");

        let (tx, rx) = mpsc::channel();
        serve(&socket, move |path| tx.send(path).unwrap()).unwrap();
        assert!(hand_over(&socket, &Ask::Show(Some("/notes/a.md".into()))));
        assert!(hand_over(&socket, &Ask::Show(None)));
        assert!(hand_over(&socket, &Ask::Theme));
        let wait = Duration::from_secs(2);
        assert_eq!(
            rx.recv_timeout(wait).unwrap(),
            Ask::Show(Some("/notes/a.md".into()))
        );
        assert_eq!(rx.recv_timeout(wait).unwrap(), Ask::Show(None));
        assert_eq!(rx.recv_timeout(wait).unwrap(), Ask::Theme);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
