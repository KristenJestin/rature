use std::path::{Path, PathBuf};

use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// Watches one file. Its folder is watched, not the file: an editor or an agent often writes a
/// new file and renames it over the old one, which a watch on the file itself loses. Each change
/// sends one message; the receiver reads the file again.
pub fn file(path: &Path) -> notify::Result<(RecommendedWatcher, UnboundedReceiver<()>)> {
    let (tx, rx) = unbounded();
    let target: PathBuf = path.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !event.kind.is_access()
            && event.paths.contains(&target)
        {
            let _ = tx.unbounded_send(());
        }
    })?;
    let folder = path.parent().unwrap_or(Path::new("/"));
    watcher.watch(folder, RecursiveMode::NonRecursive)?;
    Ok((watcher, rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn changed(rx: &mut UnboundedReceiver<()>) -> bool {
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            if rx.try_recv().is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_file_written_in_place_or_renamed_over_is_seen_each_time() {
        let folder = std::env::temp_dir().join(format!("rature-watch-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("doc.md");
        std::fs::write(&file, "# One\n").unwrap();
        let (_watcher, mut rx) = super::file(&file).unwrap();

        std::fs::write(&file, "# Two\n").unwrap();
        assert!(changed(&mut rx), "written in place");
        while rx.try_recv().is_ok() {}

        let next = folder.join(".doc.md.new");
        std::fs::write(&next, "# Three\n").unwrap();
        while rx.try_recv().is_ok() {}
        std::fs::rename(&next, &file).unwrap();
        assert!(changed(&mut rx), "renamed over");
        while rx.try_recv().is_ok() {}

        std::fs::write(folder.join("other.md"), "# Other\n").unwrap();
        assert!(!changed(&mut rx), "another file of the folder");
        std::fs::remove_dir_all(folder).unwrap();
    }
}
