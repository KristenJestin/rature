//! Start-up timings, asked for with `RATURE_TIMING`: marks taken along the way, printed in one
//! block on the standard error once the window has shown its first frame. Without it, a mark is
//! one atomic load.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// When `main` was entered, set only when the timings are asked for.
static STARTED: OnceLock<Instant> = OnceLock::new();
/// What happened, when (since `main`), and how long it took when it is a span.
static MARKS: Mutex<Vec<(&'static str, Duration, Option<Duration>)>> = Mutex::new(Vec::new());
/// The block is printed: later marks are not kept.
static PRINTED: AtomicBool = AtomicBool::new(false);
/// How many things the block still waits for before it is printed.
static AWAITED: AtomicUsize = AtomicUsize::new(0);
/// How long the process ran before `main`, as the kernel counts it (to its 10 ms tick).
static BEFORE_MAIN: OnceLock<Option<Duration>> = OnceLock::new();

/// Starts the timings, `started` being when `main` was entered.
pub fn start(started: Instant) {
    let _ = BEFORE_MAIN.set(before_main());
    let _ = STARTED.set(started);
}

pub fn on() -> bool {
    STARTED.get().is_some()
}

/// When `main` was entered, when the timings are on.
pub fn started() -> Option<Instant> {
    STARTED.get().copied()
}

/// `what` happens now.
pub fn mark(what: &'static str) {
    if let Some(started) = STARTED.get() {
        keep(what, started.elapsed(), None);
    }
}

/// `what` took `took`, and ends now.
pub fn span(what: &'static str, took: Duration) {
    if let Some(started) = STARTED.get() {
        keep(what, started.elapsed(), Some(took));
    }
}

/// Times `work` as `what`.
pub fn time<T>(what: &'static str, work: impl FnOnce() -> T) -> T {
    if !on() {
        return work();
    }
    let begun = Instant::now();
    let done = work();
    span(what, begun.elapsed());
    done
}

fn keep(what: &'static str, at: Duration, took: Option<Duration>) {
    if PRINTED.load(Ordering::Relaxed) {
        return;
    }
    MARKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((what, at, took));
}

/// The block is printed once `count` things have ended (`end`).
pub fn expect(count: usize) {
    AWAITED.store(count, Ordering::Relaxed);
}

/// One of the things the block waits for has ended: the last one prints it.
pub fn end() {
    let left = AWAITED.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
    if left == Ok(1) {
        print();
    }
}

/// Prints the marks taken so far, once.
fn print() {
    if !on() || PRINTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let marks = std::mem::take(&mut *MARKS.lock().unwrap_or_else(PoisonError::into_inner));
    let ms = |d: Duration| d.as_secs_f64() * 1000.;
    let mut out = String::from("rature: start-up, in ms since main (+ since the mark above)\n");
    match BEFORE_MAIN.get().copied().flatten() {
        Some(before) => out.push_str(&format!(
            "  {:>8}  {:>7}  process start to main (kernel ticks: ±10 ms)\n",
            format!("-{:.0}", ms(before)),
            ""
        )),
        None => out.push_str("  process start to main: unknown\n"),
    }
    let mut last = Duration::ZERO;
    for (what, at, took) in marks {
        let took = took.map_or(String::new(), |t| format!(" (took {:.2})", ms(t)));
        out.push_str(&format!(
            "  {:>8.1}  {:>+7.1}  {what}{took}\n",
            ms(at),
            ms(at.saturating_sub(last))
        ));
        last = at;
    }
    eprint!("{out}");
}

/// How long ago the process started, from `/proc`: its start and the time since boot, both in
/// the kernel's 10 ms ticks.
fn before_main() -> Option<Duration> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // The fields after the command's name, which may hold spaces, closed by the last ')'.
    let after = &stat[stat.rfind(')')? + 2..];
    // The 22nd field, the 20th after the name.
    let start_ticks: f64 = after.split(' ').nth(19)?.parse().ok()?;
    let uptime: f64 = std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split(' ')
        .next()?
        .parse()
        .ok()?;
    let ago = uptime - start_ticks / 100.;
    (ago >= 0.).then(|| Duration::from_secs_f64(ago))
}
