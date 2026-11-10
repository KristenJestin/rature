//! Writing the files Rature shows, and keeps: whole or not at all (a new file renamed over the
//! old one), never over what another program wrote since the file was read, and through a link
//! to the file it points to rather than in its place.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// How many times a file that changes while it is being written is read again before giving up.
const TRIES: usize = 3;

/// What [`update`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    /// Written: the text the file held, and the text it holds now.
    Written { before: String, after: String },
    /// The edit asked for nothing to be written, given the text the file holds.
    Left(String),
    /// The file changed each time it was about to be written: nothing was.
    Busy,
}

/// What a file's content is known by: written to, or replaced by a rename, it is another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Stamp {
    fn of(meta: &Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

/// What the thing at a path is when it is not a file to read: reading a pipe or a device would
/// wait forever, or never end.
pub fn not_a_file(meta: &Metadata) -> Option<&'static str> {
    use std::os::unix::fs::FileTypeExt;
    let kind = meta.file_type();
    match () {
        _ if kind.is_file() => None,
        _ if kind.is_dir() => Some("a folder"),
        _ if kind.is_fifo() => Some("a named pipe"),
        _ if kind.is_socket() => Some("a socket"),
        _ if kind.is_char_device() || kind.is_block_device() => Some("a device"),
        _ => Some("not a regular file"),
    }
}

/// The metadata of the file at `path`, links followed, if it is a regular file.
pub fn regular(path: &Path) -> io::Result<Metadata> {
    let meta = fs::metadata(path)?;
    match not_a_file(&meta) {
        None => Ok(meta),
        Some(what) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is {what}", path.display()),
        )),
    }
}

/// Reads the text file at `path`, gives it to `edit`, and writes what `edit` returns in its
/// place. The file is read again just before it is replaced: when another program wrote it
/// meanwhile, it is read again and `edit` asked again, so its change is kept; `edit` sees the
/// latest text each time. A link is followed: the file it points to is written, the link stays.
/// The file keeps its permissions. Text that is not UTF-8 is never written over.
pub fn update(path: &Path, mut edit: impl FnMut(&str) -> Option<String>) -> io::Result<Update> {
    let target = fs::canonicalize(path)?;
    for _ in 0..TRIES {
        // Checked before it is opened: opening a pipe waits for a writer.
        regular(&target)?;
        let mut file = File::open(&target)?;
        let meta = file.metadata()?;
        let read = Stamp::of(&meta);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        // Written to while it was read: what was read is not one version of it.
        if Stamp::of(&file.metadata()?) != read {
            continue;
        }
        let before = String::from_utf8(bytes).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "it is not UTF-8 text, so it is not written",
            )
        })?;
        let Some(after) = edit(&before) else {
            return Ok(Update::Left(before));
        };
        let temp = Temp::write(&target, after.as_bytes(), Some(meta.permissions().mode()))?;
        // The last look before the rename: the file still holds what was read. Its content is
        // compared, not only its times: a clock tick can hold two writes of the same length.
        // What another program writes between this look and the rename is the one change that
        // can still be lost.
        if !holds(&target, before.as_bytes()) {
            continue;
        }
        temp.rename_over(&target)?;
        return Ok(Update::Written { before, after });
    }
    Ok(Update::Busy)
}

/// Whether the regular file at `path` holds `expected`, and nothing else.
fn holds(path: &Path, expected: &[u8]) -> bool {
    regular(path).is_ok_and(|meta| meta.len() == expected.len() as u64)
        && fs::read(path).is_ok_and(|bytes| bytes == expected)
}

/// Writes `bytes` to `path` whole, readable by its owner alone: what Rature keeps for itself.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    Temp::write(path, bytes, Some(0o600))?.rename_over(path)
}

/// A new file beside the one it will replace, removed unless it is renamed over it.
struct Temp(Option<PathBuf>);

impl Temp {
    fn write(beside: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let folder = beside.parent().unwrap_or(Path::new("."));
        let name = beside
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = folder.join(format!(
            ".{name}.rature-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let temp = Self(Some(path));
        file.write_all(bytes)?;
        if let Some(mode) = mode {
            // Set as is, not through the umask: the file keeps what it had.
            file.set_permissions(fs::Permissions::from_mode(mode & 0o7777))?;
        }
        file.sync_all()?;
        Ok(temp)
    }

    fn rename_over(mut self, target: &Path) -> io::Result<()> {
        let path = self.0.take().expect("a temp file not yet renamed");
        if let Err(error) = fs::rename(&path, target) {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        // The rename itself is kept through a crash once its folder is.
        if let Some(folder) = target.parent()
            && let Ok(folder) = File::open(folder)
        {
            let _ = folder.sync_all();
        }
        Ok(())
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own for each test, removed at its end.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("rature-fsio-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_is_replaced_whole_and_keeps_its_permissions() {
        let folder = Folder::new("whole");
        let file = folder.0.join("a.md");
        fs::write(&file, "one\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        let done = update(&file, |now| Some(now.replace("one", "two"))).unwrap();
        assert_eq!(
            done,
            Update::Written {
                before: "one\n".into(),
                after: "two\n".into()
            }
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "two\n");
        assert_eq!(fs::metadata(&file).unwrap().mode() & 0o7777, 0o640);
        // No temporary file is left beside it.
        assert_eq!(folder.names(), ["a.md"]);
    }

    #[test]
    fn nothing_is_written_when_the_edit_declines() {
        let folder = Folder::new("left");
        let file = folder.0.join("a.rs");
        fs::write(&file, "theirs").unwrap();
        let done = update(&file, |now| (now == "mine as read").then(|| "mine".into())).unwrap();
        assert_eq!(done, Update::Left("theirs".into()));
        assert_eq!(fs::read_to_string(&file).unwrap(), "theirs");
    }

    #[test]
    fn a_write_made_meanwhile_is_read_again_and_kept() {
        let folder = Folder::new("race");
        let file = folder.0.join("a.md");
        fs::write(&file, "# A\n\nmine\n").unwrap();
        let mut asked = 0;
        let done = update(&file, |now| {
            asked += 1;
            if asked == 1 {
                // Another program writes between Rature's read and its rename.
                fs::write(&file, "# A\n\nmine\n\ntheirs\n").unwrap();
            }
            Some(now.replacen("mine", "MINE", 1))
        })
        .unwrap();
        assert_eq!(asked, 2);
        assert!(matches!(done, Update::Written { .. }));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "# A\n\nMINE\n\ntheirs\n"
        );
        assert_eq!(folder.names(), ["a.md"]);
    }

    #[test]
    fn a_file_that_never_stops_changing_is_left_alone() {
        let folder = Folder::new("busy");
        let file = folder.0.join("a.md");
        fs::write(&file, "0").unwrap();
        let mut n = 0;
        let done = update(&file, |_| {
            n += 1;
            fs::write(&file, format!("theirs {n} and more")).unwrap();
            Some("mine".into())
        })
        .unwrap();
        assert_eq!(done, Update::Busy);
        assert!(fs::read_to_string(&file).unwrap().starts_with("theirs"));
        assert_eq!(folder.names(), ["a.md"]);
    }

    #[test]
    fn a_link_stays_a_link_and_its_file_is_written() {
        let folder = Folder::new("link");
        let file = folder.0.join("real.md");
        let link = folder.0.join("link.md");
        fs::write(&file, "old").unwrap();
        std::os::unix::fs::symlink("real.md", &link).unwrap();
        update(&link, |_| Some("new".into())).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
    }

    #[test]
    fn text_that_is_not_utf8_is_never_written_over() {
        let folder = Folder::new("utf8");
        let file = folder.0.join("a.txt");
        fs::write(&file, b"caf\xe9\n").unwrap();
        let error = update(&file, |now| Some(now.to_string())).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&file).unwrap(), b"caf\xe9\n");
    }

    #[test]
    fn a_pipe_is_refused_without_waiting() {
        let folder = Folder::new("pipe");
        let pipe = folder.0.join("p");
        let made = std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap();
        assert!(made.success());
        assert_eq!(
            not_a_file(&fs::metadata(&pipe).unwrap()),
            Some("a named pipe")
        );
        let error = update(&pipe, |_| Some(String::new())).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            not_a_file(&fs::metadata("/dev/null").unwrap()),
            Some("a device")
        );
    }

    #[test]
    fn a_private_file_is_its_owners_alone() {
        let folder = Folder::new("private");
        let file = folder.0.join("state.json");
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&file, b"new").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        assert_eq!(fs::metadata(&file).unwrap().mode() & 0o777, 0o600);
        assert_eq!(folder.names(), ["state.json"]);
    }
}
