use crate::downgrade::{self, Slot};
use crate::rt;
use crate::trace;

use std::sync::atomic::Ordering;

/// Writes a trace line for the operation of the caller, see [`crate::trace`].
///
/// It must only be used in `#[track_caller]` functions, so the line is attributed to the
/// call site of the operation in the tested code.
macro_rules! trace_op {
    ($($t:tt)*) => {
        trace::write_at(::std::panic::Location::caller(), format_args!($($t)*))
    };
}

#[derive(Debug)]
pub(crate) struct Atomic<T> {
    /// Atomic object
    state: rt::Atomic<T>,
}

impl<T> Atomic<T>
where
    T: rt::Numeric,
{
    pub(crate) fn new(value: T, location: rt::Location) -> Atomic<T> {
        let state = rt::Atomic::new(value, location);

        Atomic { state }
    }

    #[track_caller]
    pub(crate) unsafe fn unsync_load(&self) -> T {
        let res = self.state.unsync_load(location!());
        trace_op!("unsync_load() -> {res:?}");
        res
    }

    #[track_caller]
    pub(crate) fn load(&self, order: Ordering) -> T {
        let order = downgrade::apply(order, Slot::Single);
        let res = self.state.load(location!(), order);
        trace_op!("load({order:?}) -> {res:?}");
        res
    }

    #[track_caller]
    pub(crate) fn store(&self, value: T, order: Ordering) {
        let order = downgrade::apply(order, Slot::Single);
        self.state.store(location!(), value, order);
        trace_op!("store({value:?}, {order:?})");
    }

    #[track_caller]
    pub(crate) fn with_mut<R>(&mut self, f: impl FnOnce(&mut T) -> R) -> R {
        self.state.with_mut(location!(), f)
    }

    /// Read-modify-write
    ///
    /// Always reads the most recent write
    ///
    /// `op` and `arg` are only used for tracing, and are the name and the argument of the
    /// operation as called by the tested code, e.g. `("fetch_add", 1)`.
    #[track_caller]
    pub(crate) fn rmw<F>(&self, op: &'static str, arg: T, f: F, order: Ordering) -> T
    where
        F: FnOnce(T) -> T,
    {
        let order = downgrade::apply(order, Slot::Single);
        let res = self
            .try_rmw::<_, ()>(order, order, |v| Ok(f(v)))
            .unwrap_or_else(|_| unreachable!());
        trace_op!("{op}({arg:?}, {order:?}) -> {res:?}");
        res
    }

    /// The orderings must have already been downgraded by the caller.
    #[track_caller]
    fn try_rmw<F, E>(&self, success: Ordering, failure: Ordering, f: F) -> Result<T, E>
    where
        F: FnOnce(T) -> Result<T, E>,
    {
        self.state.rmw(location!(), success, failure, f)
    }

    #[track_caller]
    pub(crate) fn swap(&self, val: T, order: Ordering) -> T {
        self.rmw("swap", val, |_| val, order)
    }

    #[track_caller]
    pub(crate) fn compare_and_swap(&self, current: T, new: T, order: Ordering) -> T {
        use self::Ordering::*;

        let failure = match order {
            Relaxed | Release => Relaxed,
            Acquire | AcqRel => Acquire,
            _ => SeqCst,
        };

        match self.compare_exchange_named("compare_and_swap", current, new, order, failure) {
            Ok(v) => v,
            Err(v) => v,
        }
    }

    #[track_caller]
    pub(crate) fn compare_exchange(
        &self,
        current: T,
        new: T,
        success: Ordering,
        failure: Ordering,
    ) -> Result<T, T> {
        self.compare_exchange_named("compare_exchange", current, new, success, failure)
    }

    /// `op` is only used for tracing, and is the name of the operation as called by the
    /// tested code, e.g. `"compare_exchange_weak"`.
    #[track_caller]
    pub(crate) fn compare_exchange_named(
        &self,
        op: &'static str,
        current: T,
        new: T,
        success: Ordering,
        failure: Ordering,
    ) -> Result<T, T> {
        // The two orderings of a compare-exchange share a call site, so they need a slot to
        // be selected independently.
        let success = downgrade::apply(success, Slot::Success);
        let failure = downgrade::apply(failure, Slot::Failure);
        let res = self.try_rmw(success, failure, |actual| {
            if actual == current {
                Ok(new)
            } else {
                Err(actual)
            }
        });
        trace_op!("{op}({current:?}, {new:?}, {success:?}, {failure:?}) -> {res:?}");
        res
    }

    #[track_caller]
    pub(crate) fn fetch_update<F>(
        &self,
        set_order: Ordering,
        fetch_order: Ordering,
        mut f: F,
    ) -> Result<T, T>
    where
        F: FnMut(T) -> Option<T>,
    {
        // The `load` and `compare_exchange` below are traced (and downgradable) on their
        // own, sharing the call site of this `fetch_update`.
        let mut prev = self.load(fetch_order);
        while let Some(next) = f(prev) {
            match self.compare_exchange_named(
                "compare_exchange",
                prev,
                next,
                set_order,
                fetch_order,
            ) {
                Ok(x) => return Ok(x),
                Err(next_prev) => prev = next_prev,
            }
        }
        Err(prev)
    }
}
