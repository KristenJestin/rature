//! One window: a second `rature` hands its file to the running one, over a socket in the
//! runtime folder, and exits.
//!
//! The protocol is one message per connection: the path's raw bytes, then a NUL (a path never
//! holds one). A lone NUL asks only for the window.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The longest message read: a path is far shorter.
const MOST: usize = 64 * 1024;
/// How long a connection may take to say what it asks.
const PATIENCE: Duration = Duration::from_secs(2);

/// Where the socket is: the user's runtime folder, or else a folder of the user's own in the
/// temporary one, that no other user can write in.
pub fn socket() -> PathBuf {
    if let Some(folder) = std::env::var_os("XDG_RUNTIME_DIR").filter(|f| !f.is_empty()) {
        return PathBuf::from(folder).join("rature.sock");
    }
    let temp = std::env::temp_dir();
    if let Some(uid) = uid() {
        let folder = temp.join(format!("rature-{uid}"));
        if private_folder(&folder, uid) {
            return folder.join("rature.sock");
        }
        eprintln!(
            "rature: {} is not a private folder: other windows will open beside this one",
            folder.display()
        );
    }
    // No folder to trust: a socket of this process alone, so nobody can take it over.
    let folder = temp.join(format!("rature-{}", std::process::id()));
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&folder);
    folder.join("rature.sock")
}

/// The user this process runs as: the owner of its `/proc` entry.
fn uid() -> Option<u32> {
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

/// Makes `folder` if need be, and says whether it is a real folder of `uid` that only it can use.
fn private_folder(folder: &Path, uid: u32) -> bool {
    let _ = std::fs::DirBuilder::new().mode(0o700).create(folder);
    std::fs::symlink_metadata(folder)
        .is_ok_and(|m| m.is_dir() && m.uid() == uid && m.mode() & 0o077 == 0)
}

/// What a client sends for `path`.
fn encode(path: Option<&Path>) -> Vec<u8> {
    let mut message = path
        .map(|p| p.as_os_str().as_bytes().to_vec())
        .unwrap_or_default();
    message.push(0);
    message
}

/// The path a whole message asks for (`Some(None)`: only the window); `None` while the message
/// is not whole.
fn decode(message: &[u8]) -> Option<Option<PathBuf>> {
    let end = message.iter().position(|&b| b == 0)?;
    let bytes = &message[..end];
    Some((!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))))
}

/// Brings the running Rature to the front, with `path` in it if any. False when none runs.
pub fn hand_over(socket: &Path, path: Option<&Path>) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_write_timeout(Some(PATIENCE));
    stream.write_all(&encode(path)).is_ok()
}

/// Reads one message from `stream`, giving up after `PATIENCE` or `MOST` bytes.
fn receive(mut stream: UnixStream) -> Option<Option<PathBuf>> {
    let deadline = Instant::now() + PATIENCE;
    let mut message = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        if left.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(left)).ok()?;
        match stream.read(&mut buffer) {
            Ok(0) => return None,
            Ok(read) => {
                message.extend_from_slice(&buffer[..read]);
                if let Some(asked) = decode(&message) {
                    return Some(asked);
                }
                if message.len() > MOST {
                    return None;
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

/// Takes the socket: a file left there by a dead instance is removed, a live one's is not.
fn bind(socket: &Path) -> std::io::Result<UnixListener> {
    // Two Rature starting at once take turns, so neither removes the socket the other just made.
    let mut lock = socket.as_os_str().to_owned();
    lock.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(lock))?;
    lock.lock()?;
    match UnixListener::bind(socket) {
        Err(error) if error.kind() == ErrorKind::AddrInUse => {}
        bound => return bound,
    }
    match UnixStream::connect(socket) {
        // Another Rature answers there (it started at the same time as this one).
        Ok(_) => return Err(ErrorKind::AddrInUse.into()),
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Nobody listens: a dead instance's socket.
    match std::fs::remove_file(socket) {
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    UnixListener::bind(socket)
}

/// Listens for what other `rature` ask, on a thread of its own.
pub fn serve(
    socket: &Path,
    on_ask: impl Fn(Option<PathBuf>) + Send + 'static,
) -> std::io::Result<()> {
    let listener = bind(socket)?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            // A client that says nothing is let go after a while: it cannot hold the others.
            if let Some(asked) = receive(stream) {
                on_ask(asked);
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::sync::mpsc;

    fn folder(name: &str) -> PathBuf {
        let folder =
            std::env::temp_dir().join(format!("rature-instance-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn a_second_rature_hands_its_file_to_the_first() {
        let folder = folder("hand");
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

    #[test]
    fn any_path_goes_through_whole() {
        let odd = [
            PathBuf::from("/notes/two\nlines.md"),
            PathBuf::from(OsStr::from_bytes(b"/notes/caf\xe9.md")),
            PathBuf::from("/notes/a:b.md"),
        ];
        for path in odd {
            assert_eq!(decode(&encode(Some(&path))), Some(Some(path)));
        }
        assert_eq!(decode(&encode(None)), Some(None));
        assert_eq!(
            decode(b"/notes/half"),
            None,
            "a message cut short asks nothing"
        );
    }

    #[test]
    fn a_silent_client_does_not_hold_the_others() {
        let folder = folder("silent");
        let socket = folder.join("rature.sock");
        let (tx, rx) = mpsc::channel();
        serve(&socket, move |path| tx.send(path).unwrap()).unwrap();
        let _silent = UnixStream::connect(&socket).unwrap();
        let path = PathBuf::from(OsStr::from_bytes(b"/notes/\xff\nb.md"));
        assert!(hand_over(&socket, Some(&path)));
        assert_eq!(
            rx.recv_timeout(PATIENCE * 3).unwrap(),
            Some(path),
            "heard once the silent one is let go"
        );
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_live_socket_is_kept_and_a_dead_one_replaced() {
        let folder = folder("bind");
        let socket = folder.join("rature.sock");
        // A dead instance's socket: bound, then dropped, the file left behind.
        drop(UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());
        let live = bind(&socket).expect("a dead socket is taken over");
        assert_eq!(
            bind(&socket).unwrap_err().kind(),
            ErrorKind::AddrInUse,
            "a live one is left alone"
        );
        drop(live);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn the_private_folder_refuses_one_open_to_others() {
        use std::os::unix::fs::PermissionsExt as _;
        let folder = folder("private");
        let uid = uid().unwrap();
        let mine = folder.join("mine");
        assert!(private_folder(&mine, uid));
        let open = folder.join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!private_folder(&open, uid));
        let link = folder.join("link");
        std::os::unix::fs::symlink(&mine, &link).unwrap();
        assert!(!private_folder(&link, uid), "a link may lead anywhere");
        assert!(!private_folder(&mine, uid + 1));
        std::fs::remove_dir_all(folder).unwrap();
    }
}
