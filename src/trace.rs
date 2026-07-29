//! Readable tracing of the operations performed by a model.
//!
//! This is a fork-only addition (see also [`crate::downgrade`]), unrelated to the `tracing`
//! instrumentation used by `LOOM_LOG`: setting the `LOOM_TRACE` environment variable makes
//! every atomic operation (and every [`trace!`](crate::trace!) call of the tested code) be
//! written as a single line into a `loom.trace` file, in the current directory:
//!
//! ```text
//! ============================== iteration 42 ==============================
//! ================================ thread 0 ================================
//! src/lib.rs:393:36: store(3, SeqCst)
//! ================================ thread 1 ================================
//! src/lib.rs:506:36: fence(SeqCst)
//! src/lib.rs:506:36: load(SeqCst) -> 3
//! ```
//!
//! The file is truncated at the beginning of each iteration, so it only ever contains the
//! last one — which is the failing one when the model panics.
//!
//! Lines are prefixed by the location of the operation, and *not* by the thread performing
//! it: instead, a separator line is written each time the traced thread changes. Because it
//! is only written when the *traced* thread changes, a thread switch happening between two
//! untraced operations doesn't show up.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::panic::Location;
use std::sync::{Mutex, OnceLock};

const PATH: &str = "loom.trace";
const VAR: &str = "LOOM_TRACE";
/// Width of the separator lines, chosen to fit in a terminal.
const WIDTH: usize = 74;

struct Trace {
    file: File,
    /// Thread of the last written line, to write a separator when it changes.
    ///
    /// `None` means "no line written since the last iteration started", and not "outside of
    /// a model": a separator is written for the first line of each iteration.
    last_thread: Option<usize>,
}

fn trace() -> Option<&'static Mutex<Trace>> {
    static TRACE: OnceLock<Option<Mutex<Trace>>> = OnceLock::new();
    TRACE
        .get_or_init(|| {
            std::env::var_os(VAR)?;
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(PATH)
                .unwrap();
            Some(Mutex::new(Trace {
                file,
                last_thread: None,
            }))
        })
        .as_ref()
}

/// Whether tracing is enabled, i.e. whether `LOOM_TRACE` is set.
pub fn enabled() -> bool {
    trace().is_some()
}

/// Writes a `= label =` separator line.
fn separator(file: &mut File, label: fmt::Arguments<'_>) {
    let label = format!(" {label} ");
    let padding = WIDTH.saturating_sub(label.len());
    let (left, right) = (padding / 2, padding - padding / 2);
    let bar = |n: usize| "=".repeat(n);
    writeln!(file, "{}{label}{}", bar(left), bar(right)).unwrap();
}

/// Truncates the trace, so it only contains the iteration which is about to start.
pub(crate) fn new_iteration(iteration: usize) {
    let Some(trace) = trace() else { return };
    // Poisoning is expected: a model panics, and it does it a lot when checking orderings.
    let mut trace = trace.lock().unwrap_or_else(|err| err.into_inner());
    let trace = &mut *trace;
    trace.file.set_len(0).unwrap();
    trace.file.rewind().unwrap();
    trace.last_thread = None;
    separator(&mut trace.file, format_args!("iteration {iteration}"));
}

/// Writes a trace line for an operation performed at `location`.
pub(crate) fn write_at(location: &Location<'_>, args: fmt::Arguments<'_>) {
    let Some(trace) = trace() else { return };
    let thread = crate::rt::traced_thread();
    let mut trace = trace.lock().unwrap_or_else(|err| err.into_inner());
    let trace = &mut *trace;
    if trace.last_thread != thread {
        trace.last_thread = thread;
        match thread {
            Some(thread) => separator(&mut trace.file, format_args!("thread {thread}")),
            // Not inside a model, or the execution state is already borrowed (which the
            // caller of `trace!` cannot really be blamed for).
            None => separator(&mut trace.file, format_args!("unknown thread")),
        }
    }
    writeln!(trace.file, "{location}: {args}").unwrap();
    // Flushed on each line: the interesting trace is the one of a panicking iteration.
    trace.file.flush().unwrap();
}

/// Writes a line into the trace file, prefixed by the location of the call.
///
/// The line is only written if `LOOM_TRACE` is set; the arguments are not even formatted
/// otherwise. It is meant to be used by the tested code to trace its own events, in order
/// to have them interleaved with the atomic operations, e.g.
///
/// ```ignore
/// loom::trace!("Waker::wake");
/// ```
///
/// `core::format_args!` is used rather than `std::format_args!`, so the macro can be called
/// from a `no_std` crate (which loom itself is not).
#[macro_export]
macro_rules! trace {
    ($($t:tt)*) => {
        $crate::trace::write(::core::format_args!($($t)*))
    };
}

/// Implementation detail of [`trace!`](crate::trace!).
#[doc(hidden)]
#[track_caller]
pub fn write(args: fmt::Arguments<'_>) {
    write_at(Location::caller(), args);
}
