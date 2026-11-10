use std::collections::HashSet;
use std::path::{Path, PathBuf};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// Watches one file. Its folder is watched, not the file: an editor or an agent often writes a
/// new file and renames it over the old one, which a watch on the file itself loses. A link is
/// followed: the folder of the file it leads to is watched too, so a write to that file is seen.
/// Each change sends one message; the receiver reads the file again.
pub fn file(path: &Path) -> notify::Result<(RecommendedWatcher, UnboundedReceiver<()>)> {
    let (tx, rx) = unbounded();
    let targets = targets(path);
    let matched = targets.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !event.kind.is_access()
            && event.paths.iter().any(|p| matched.contains(p))
        {
            let _ = tx.unbounded_send(());
        }
    })?;
    let folders: HashSet<&Path> = targets
        .iter()
        .map(|t| t.parent().unwrap_or(Path::new("/")))
        .collect();
    for folder in folders {
        watcher.watch(folder, RecursiveMode::NonRecursive)?;
    }
    Ok((watcher, rx))
}

/// The paths a change to `path` shows at: itself, and the file it leads to when it is a link.
fn targets(path: &Path) -> Vec<PathBuf> {
    let mut targets = vec![path.to_path_buf()];
    if let Ok(real) = std::fs::canonicalize(path)
        && real != path
    {
        targets.push(real);
    }
    targets
}

/// Watches the folders a tree shows open, each alone (not what it holds): a folder that is not
/// open costs no watch, so opening `~` does not watch all of it. The watches are added and
/// removed off the main thread, in the order they were asked for.
pub struct Folders {
    watched: HashSet<PathBuf>,
    plans: Option<UnboundedSender<Plan>>,
}

/// What to change in the folders watched.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub add: Vec<PathBuf>,
    pub remove: Vec<PathBuf>,
}

impl Plan {
    /// From watching `now` to watching `wanted`, in a stable order.
    pub fn between(now: &HashSet<PathBuf>, wanted: &HashSet<PathBuf>) -> Self {
        let mut add: Vec<PathBuf> = wanted.difference(now).cloned().collect();
        let mut remove: Vec<PathBuf> = now.difference(wanted).cloned().collect();
        add.sort();
        remove.sort();
        Self { add, remove }
    }

    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty()
    }
}

impl Folders {
    /// A watcher with nothing watched yet; the paths each change touched come on the receiver.
    /// `work` is to run off the main thread: it adds and removes the watches as `set` asks.
    pub fn new() -> notify::Result<(
        Self,
        UnboundedReceiver<Vec<PathBuf>>,
        impl Future<Output = ()> + Send + 'static,
    )> {
        let (watcher, rx) = watcher()?;
        let (plans, mut asked) = unbounded::<Plan>();
        let work = async move {
            use futures::StreamExt as _;
            let mut watcher = watcher;
            while let Some(plan) = asked.next().await {
                apply(&mut watcher, &plan);
            }
        };
        Ok((
            Self {
                watched: HashSet::new(),
                plans: Some(plans),
            },
            rx,
            work,
        ))
    }

    /// Watches `wanted` and nothing else: what changed is sent to the worker.
    pub fn set(&mut self, wanted: HashSet<PathBuf>) -> Plan {
        let plan = Plan::between(&self.watched, &wanted);
        self.watched = wanted;
        if !plan.is_empty()
            && let Some(plans) = &self.plans
        {
            let _ = plans.unbounded_send(Plan {
                add: plan.add.clone(),
                remove: plan.remove.clone(),
            });
        }
        plan
    }
}

/// A watcher that sends the paths each change touched.
fn watcher() -> notify::Result<(RecommendedWatcher, UnboundedReceiver<Vec<PathBuf>>)> {
    let (tx, rx) = unbounded();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !event.kind.is_access()
        {
            let _ = tx.unbounded_send(event.paths);
        }
    })?;
    Ok((watcher, rx))
}

/// Adds and removes the watches of `plan`. A folder that cannot be watched (gone, or the
/// system's watches used up) is said and left: the others, and the open file's own watch, go on.
fn apply(watcher: &mut impl Watcher, plan: &Plan) {
    for folder in &plan.remove {
        // A folder removed from disk lost its watch with it.
        let _ = watcher.unwatch(folder);
    }
    for folder in &plan.add {
        if let Err(error) = watcher.watch(folder, RecursiveMode::NonRecursive) {
            eprintln!("rature: {}: not followed: {error}", folder.display());
        }
    }
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

    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("rature-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn a_file_written_in_place_or_renamed_over_is_seen_each_time() {
        let folder = folder("watch");
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

    #[test]
    fn a_link_is_followed_to_its_file() {
        let root = folder("watch-link");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::create_dir_all(root.join("links")).unwrap();
        let target = root.join("real/doc.md");
        std::fs::write(&target, "# One\n").unwrap();
        let link = root.join("links/doc.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let (_watcher, mut rx) = super::file(&link).unwrap();

        std::fs::write(&target, "# Two\n").unwrap();
        assert!(changed(&mut rx), "the file the link leads to");
        std::fs::remove_dir_all(root).unwrap();
    }

    fn set(paths: &[&Path]) -> HashSet<PathBuf> {
        paths.iter().map(|p| p.to_path_buf()).collect()
    }

    #[test]
    fn a_plan_adds_what_opened_and_removes_what_closed() {
        let (a, b, c) = (Path::new("/r"), Path::new("/r/b"), Path::new("/r/c"));
        let plan = Plan::between(&set(&[a, b]), &set(&[a, c]));
        assert_eq!(plan.add, [c]);
        assert_eq!(plan.remove, [b]);
        assert!(Plan::between(&set(&[a]), &set(&[a])).is_empty());
    }

    #[test]
    fn only_the_open_folders_are_watched_each_alone() {
        let root = folder("watch-folders");
        std::fs::create_dir_all(root.join("open/deep")).unwrap();
        std::fs::create_dir_all(root.join("shut")).unwrap();
        // What `Folders::set` hands its worker, applied here in turn.
        let (mut watcher, mut rx) = watcher().unwrap();
        let mut watched = HashSet::new();
        let mut set_to = |wanted: HashSet<PathBuf>| {
            let plan = Plan::between(&watched, &wanted);
            apply(&mut watcher, &plan);
            watched = wanted;
            plan
        };
        set_to(set(&[&root, &root.join("open")]));
        let seen = |rx: &mut UnboundedReceiver<Vec<PathBuf>>, path: &Path| {
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until {
                while let Ok(paths) = rx.try_recv() {
                    if paths.iter().any(|p| p == path) {
                        return true;
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        };
        std::fs::write(root.join("open/a.md"), "a").unwrap();
        assert!(seen(&mut rx, &root.join("open/a.md")), "in an open folder");
        std::fs::write(root.join("shut/b.md"), "b").unwrap();
        assert!(
            !seen(&mut rx, &root.join("shut/b.md")),
            "in a folder not open"
        );
        std::fs::write(root.join("open/deep/c.md"), "c").unwrap();
        assert!(
            !seen(&mut rx, &root.join("open/deep/c.md")),
            "not recursive"
        );

        let plan = set_to(set(&[&root]));
        assert_eq!(plan.remove, [root.join("open")]);
        while rx.try_recv().is_ok() {}
        std::fs::write(root.join("open/d.md"), "d").unwrap();
        assert!(!seen(&mut rx, &root.join("open/d.md")), "a folder closed");
        std::fs::remove_dir_all(root).unwrap();
    }
}
