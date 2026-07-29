//! Mock implementation of `std::sync::atomic`.

use crate::downgrade::Slot;

#[allow(clippy::module_inception)]
mod atomic;
use self::atomic::Atomic;

mod bool;
pub use self::bool::AtomicBool;

mod int;
pub use self::int::{AtomicI16, AtomicI32, AtomicI8, AtomicIsize};
pub use self::int::{AtomicU16, AtomicU32, AtomicU8, AtomicUsize};

#[cfg(target_has_atomic = "64")]
pub use self::int::{AtomicI64, AtomicU64};

mod ptr;
pub use self::ptr::AtomicPtr;

#[doc(no_inline)]
pub use std::sync::atomic::Ordering;

/// Signals the processor that it is entering a busy-wait spin-loop.
///
/// For loom, this is an alias of [`yield_now`] but is provided as a reflection
/// of the deprecated [`core::sync::atomic::spin_loop_hint`] function. See the
/// [`yield_now`] documentation for more information on what effect using this
/// has on loom.
///
/// [`yield_now`]: crate::thread::yield_now
pub fn spin_loop_hint() {
    crate::thread::yield_now();
}

/// An atomic fence.
///
/// A fence is downgradable like any atomic operation, see [`crate::downgrade`].
#[track_caller]
pub fn fence(order: Ordering) {
    let location = std::panic::Location::caller();
    let downgraded = crate::downgrade::apply(order, Slot::Single);
    // There is no such thing as a relaxed fence, so a fence downgraded to `Relaxed` is
    // removed. An ordering which was already `Relaxed` is not downgraded, and still panics
    // in `rt::fence`.
    if downgraded == Ordering::Relaxed && order != Ordering::Relaxed {
        crate::trace::write_at(location, format_args!("fence({order:?}) [removed]"));
        return;
    }
    crate::rt::fence(downgraded);
    crate::trace::write_at(location, format_args!("fence({downgraded:?})"));
}
