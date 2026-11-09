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

/// Gives `path` (or nothing: the window only comes to the front) to the running Rature. False
/// when none runs.
pub fn hand_over(socket: &Path, path: Option<&Path>) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let line = path
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    writeln!(stream, "{line}").is_ok()
}

/// Listens for the files other `rature` hand over, on a thread of its own: `on_open` gets each
/// one, `None` when only the window is asked for.
pub fn serve(
    socket: &Path,
    on_open: impl Fn(Option<PathBuf>) + Send + 'static,
) -> std::io::Result<()> {
    // Nobody answered `hand_over`: a socket left there is a dead instance's.
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_ok() {
                let line = line.trim_end_matches('\n');
                on_open((!line.is_empty()).then(|| PathBuf::from(line)));
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
        assert!(!hand_over(&socket, None), "nobody listens yet");

        let (tx, rx) = mpsc::channel();
        serve(&socket, move |path| tx.send(path).unwrap()).unwrap();
        assert!(hand_over(&socket, Some(Path::new("/notes/a.md"))));
        assert!(hand_over(&socket, None));
        let wait = Duration::from_secs(2);
        assert_eq!(
            rx.recv_timeout(wait).unwrap(),
            Some(PathBuf::from("/notes/a.md"))
        );
        assert_eq!(rx.recv_timeout(wait).unwrap(), None);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
