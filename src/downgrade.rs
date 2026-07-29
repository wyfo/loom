//! Runtime downgrading of the memory orderings of the tested code.
//!
//! This is a fork-only addition (see also [the `trace` module](mod@crate::trace)). It makes
//! it possible to check that the orderings used by the tested code are *minimal*: weaken one
//! of them, and if no test fails, that ordering may be unnecessary — or the test suite is
//! missing a case.
//!
//! Nothing is weakened by editing the tested code, it is done at runtime, by selecting a
//! single operation with the `LOOM_DOWNGRADE` environment variable:
//!
//! ```text
//! LOOM_DOWNGRADE=<file>:<line>:<column>[:<slot>]:<from>:<to>
//! ```
//!
//! An operation is selected by its *call site*, which is the location `#[track_caller]`
//! reports for it, plus its current ordering `from` — the same site can be executed with
//! several orderings, e.g. `atomic.load(if cached { Acquire } else { Relaxed })`. `slot`
//! disambiguates the two orderings of a compare-exchange: `success` or `failure`, absent for
//! every other operation.
//!
//! A fence is an operation like any other, downgrading it to a weaker ordering; as there is
//! no such thing as a relaxed fence, downgrading a fence to `Relaxed` *removes* it.
//!
//! Only the operations of the library under test are collected, i.e. those whose call site is
//! relative to its `src` directory: the orderings of its tests are not the subject of the
//! check, and a dependency (loom included) is reported with an absolute path anyway.
//!
//! `Relaxed` operations are neither downgradable nor collected, as they cannot be weakened.
//!
//! # Collecting the downgrades to check
//!
//! Setting `LOOM_DOWNGRADE=collect` downgrades nothing; it makes instead every executed
//! downgradable operation record itself into a `loom_downgrade.sh` file, which is a script
//! running the command given as arguments once per possible downgrade, e.g.
//!
//! ```text
//! ./loom_downgrade.sh cargo test --release
//! ```
//!
//! Each downgrade is printed as the `LOOM_DOWNGRADE` value selecting it, so a single run
//! reproduces it, and marked on the same line once the command is done: ✓ when the downgrade
//! was caught, ✗ when it was not, i.e. when the ordering may be unnecessary. Note that only the
//! operations *executed* are collected, so the script is only as exhaustive as the test suite it
//! is collected from.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Seek, Write};
use std::panic::Location;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

const VAR: &str = "LOOM_DOWNGRADE";
const COLLECT_MODE: &str = "collect";
const COLLECT_PATH: &str = "loom_downgrade.sh";
/// The directory of the code whose orderings are checked, i.e. the library itself.
const SCOPE: &str = "src";

/// Which ordering of an operation is downgraded.
///
/// A compare-exchange has two orderings at a single call site, so its site is not enough to
/// select one of them; every other operation has only one, and uses [`Slot::Single`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Slot {
    /// The only ordering of the operation, not written in the `LOOM_DOWNGRADE` format.
    Single,
    /// The success ordering of a compare-exchange, written `success`.
    Success,
    /// The failure ordering of a compare-exchange, written `failure`.
    Failure,
}

impl Slot {
    fn parse(s: &str) -> Option<Slot> {
        match s {
            "success" => Some(Slot::Success),
            "failure" => Some(Slot::Failure),
            _ => None,
        }
    }

    /// The slot as a `LOOM_DOWNGRADE` field, *including* its trailing `:`, so
    /// [`Slot::Single`] can be written as nothing at all.
    fn field(self) -> &'static str {
        match self {
            Slot::Single => "",
            Slot::Success => "success:",
            Slot::Failure => "failure:",
        }
    }
}

/// The call site of a downgradable operation, i.e. the location `#[track_caller]` reports
/// for it. The column disambiguates two operations sharing a line, and the file prevents a
/// line number from matching in another one.
struct Site {
    file: String,
    line: u32,
    column: u32,
}

impl Site {
    /// Parses a `<file>:<line>:<column>` site. It parses from the right, because a Windows
    /// path can contain a ':'.
    fn parse(s: &str) -> Site {
        let invalid = || -> ! { panic!("invalid site in {VAR}: {s:?}") };
        let (rest, column) = s.rsplit_once(':').unwrap_or_else(|| invalid());
        let (file, line) = rest.rsplit_once(':').unwrap_or_else(|| invalid());
        Site {
            file: file.into(),
            line: line.parse().unwrap_or_else(|_| invalid()),
            column: column.parse().unwrap_or_else(|_| invalid()),
        }
    }

    fn matches(&self, location: &Location<'_>) -> bool {
        self.line == location.line()
            && self.column == location.column()
            && self.file == location.file()
    }
}

/// A collected operation, keyed by everything needed to select it. The file and the ordering
/// are stored as `&'static str` to avoid allocating on each operation.
type Collected = (&'static str, u32, u32, Slot, &'static str);

enum Downgrade {
    /// The `loom_downgrade.sh` file, and the operations already written to it.
    Collect(Mutex<(File, BTreeSet<Collected>)>),
    Ordering {
        site: Site,
        slot: Slot,
        from: Ordering,
        to: Ordering,
    },
}

fn downgrade() -> Option<&'static Downgrade> {
    static DOWNGRADE: OnceLock<Option<Downgrade>> = OnceLock::new();
    DOWNGRADE
        .get_or_init(|| {
            let var = std::env::var_os(VAR)?.into_string().unwrap();
            if var == COLLECT_MODE {
                let file = File::create(COLLECT_PATH).unwrap();
                // The script is meant to be run as `./loom_downgrade.sh <command>`.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
                return Some(Downgrade::Collect(Mutex::new((file, BTreeSet::new()))));
            }
            let invalid = || -> ! { panic!("invalid {VAR}: {var:?}") };
            let (rest, to) = var.rsplit_once(':').unwrap_or_else(|| invalid());
            let (rest, from) = rest.rsplit_once(':').unwrap_or_else(|| invalid());
            // The slot is optional, and a site always ends with `:<column>`, so what
            // precedes `from` is a slot if and only if it is not a number.
            let (site, slot) = match rest.rsplit_once(':') {
                Some((site, slot)) => match Slot::parse(slot) {
                    Some(slot) => (site, slot),
                    None if slot.parse::<u32>().is_ok() => (rest, Slot::Single),
                    None => invalid(),
                },
                None => invalid(),
            };
            Some(Downgrade::Ordering {
                site: Site::parse(site),
                slot,
                from: parse_ordering(from).unwrap_or_else(|| invalid()),
                to: parse_ordering(to).unwrap_or_else(|| invalid()),
            })
        })
        .as_ref()
}

fn parse_ordering(s: &str) -> Option<Ordering> {
    Some(match s {
        "Relaxed" => Ordering::Relaxed,
        "Acquire" => Ordering::Acquire,
        "Release" => Ordering::Release,
        "AcqRel" => Ordering::AcqRel,
        "SeqCst" => Ordering::SeqCst,
        _ => return None,
    })
}

fn ordering_name(order: Ordering) -> &'static str {
    match order {
        Ordering::Relaxed => "Relaxed",
        Ordering::Acquire => "Acquire",
        Ordering::Release => "Release",
        Ordering::AcqRel => "AcqRel",
        Ordering::SeqCst => "SeqCst",
        _ => unreachable!(),
    }
}

/// The orderings an ordering can be downgraded to, one step weaker only: `Acquire` and
/// `Release` together are weaker than `AcqRel` in both directions, so if downgrading to them
/// is caught, so is downgrading to `Relaxed`. `Relaxed` is nonetheless a downgrade of
/// `Acquire`/`Release`, and removes a fence.
fn weaker(order: Ordering) -> &'static [Ordering] {
    match order {
        Ordering::SeqCst => &[Ordering::AcqRel, Ordering::Acquire, Ordering::Release],
        Ordering::AcqRel => &[Ordering::Acquire, Ordering::Release],
        Ordering::Acquire | Ordering::Release => &[Ordering::Relaxed],
        _ => &[],
    }
}

/// Whether the operation belongs to the library under test.
fn in_scope(file: &str) -> bool {
    Path::new(file).starts_with(SCOPE)
}

/// Rewrites the whole `loom_downgrade.sh` script, rather than appending to it: it keeps the
/// downgrades sorted whatever the execution order, and the script exhaustive even if the run
/// is interrupted — a full suite run is long, and a panicking one doesn't reach its end.
fn write_script(file: &mut File, collected: &BTreeSet<Collected>) {
    file.set_len(0).unwrap();
    file.rewind().unwrap();
    write!(file, "{SCRIPT_HEADER}").unwrap();
    // Cargo fingerprints `RUSTFLAGS` as a raw string, so a different value would rebuild the
    // whole dependency graph. Running loom tests requires it (`--cfg loom`), so it should be
    // set, but it may come from a cargo configuration instead of the environment.
    match std::env::var("RUSTFLAGS") {
        Ok(rustflags) => {
            // Only used as a default, so the caller can still choose its own value, but an empty
            // value is overwritten: it cannot be the deliberate one, as running loom tests
            // requires at least `--cfg loom`.
            // The default is expanded as if double-quoted, being inside double quotes.
            let mut escaped = String::new();
            for c in rustflags.chars() {
                if matches!(c, '\\' | '"' | '$' | '`') {
                    escaped.push('\\');
                }
                escaped.push(c);
            }
            writeln!(file, "export RUSTFLAGS=\"${{RUSTFLAGS:-{escaped}}}\"").unwrap();
        }
        Err(_) => writeln!(file, "# RUSTFLAGS was not set in the collecting run.").unwrap(),
    }
    let mut downgrades = Vec::new();
    for &(path, line, column, slot, from) in collected {
        for to in weaker(parse_ordering(from).unwrap()) {
            let to = ordering_name(*to);
            downgrades.push(format!(
                "{path}:{line}:{column}:{}{from}:{to}",
                slot.field()
            ));
        }
    }
    // Width of the downgrade column, so the marks are aligned whatever the line numbers.
    let width = downgrades.iter().map(String::len).max().unwrap_or(0);
    writeln!(file, "\nwidth={width}\n\ndowngrades=(").unwrap();
    for downgrade in &downgrades {
        writeln!(file, "    '{downgrade}'").unwrap();
    }
    writeln!(file, ")").unwrap();
    write!(file, "{SCRIPT_BODY}").unwrap();
    file.flush().unwrap();
}

/// Records an executed downgradable operation.
fn collect(
    collected: &Mutex<(File, BTreeSet<Collected>)>,
    location: &'static Location<'static>,
    slot: Slot,
    order: Ordering,
) {
    if !in_scope(location.file()) {
        return;
    }
    // Poisoning is expected: a model panics, and it does it a lot when checking orderings.
    let mut guard = collected.lock().unwrap_or_else(|err| err.into_inner());
    let (file, collected) = &mut *guard;
    let entry = (
        location.file(),
        location.line(),
        location.column(),
        slot,
        ordering_name(order),
    );
    if collected.insert(entry) {
        write_script(file, collected);
    }
}

/// Returns the ordering to actually use for the operation of the caller, which is the given
/// one unless it is selected by `LOOM_DOWNGRADE`.
///
/// A fence downgraded to `Relaxed` must be removed by its caller; a fence is the only
/// operation whose returned ordering can be `Relaxed` while the given one was not.
#[track_caller]
pub(crate) fn apply(order: Ordering, slot: Slot) -> Ordering {
    // Relaxed cannot be downgraded, so it is neither collected nor matched.
    if order == Ordering::Relaxed {
        return order;
    }
    let location = Location::caller();
    match downgrade() {
        Some(Downgrade::Collect(collected)) => collect(collected, location, slot, order),
        Some(Downgrade::Ordering {
            site,
            slot: s,
            from,
            to,
        }) if *from == order && *s == slot && site.matches(location) => return *to,
        _ => {}
    }
    order
}

const SCRIPT_HEADER: &str = r#"#!/usr/bin/env bash
# Generated by loom during a `LOOM_DOWNGRADE=collect` run, see `loom::downgrade`.
#
# Runs the command given as arguments once per downgrade of the memory orderings executed by
# the collecting run, e.g.
#
#     ./loom_downgrade.sh cargo test --release
#
# Each downgrade is printed as the `LOOM_DOWNGRADE` value selecting it, so a single run
# reproduces it, then marked once the command is done: ✓ if it failed, meaning the downgrade
# was caught, ✗ if it passed, meaning the ordering may be unnecessary. The exit status is
# non-zero if any downgrade was not caught.
set -uo pipefail

"#;

const SCRIPT_BODY: &str = r#"

if (($# == 0)); then
    echo "usage: $0 <command> [<args>...]" >&2
    exit 2
fi

uncaught=0
for downgrade in "${downgrades[@]}"; do
    # Printed before the command runs, which takes a while, so the downgrade being checked is
    # visible while it is checked, and marked once it is done.
    printf '%-*s ' "$width" "$downgrade"
    LOOM_DOWNGRADE="$downgrade" "$@" > /dev/null 2>&1
    status=$?
    # A panic from a downgraded ordering is caught and reported as a test failure, so the only
    # expected outcomes are libtest's 101 and a clean pass. Anything else (e.g. a stack
    # overflow from a divergent loom execution) is a real problem, not a "catch": abort rather
    # than silently reading it as coverage.
    case $status in
    0) echo '✗' && uncaught=1 ;;
    101) echo '✓' ;;
    *) echo "the command crashed (exit $status)" && exit 1 ;;
    esac
done

exit $uncaught
"#;
