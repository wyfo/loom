use super::UnsafeCell;

/// A checked version of [`std::cell::Cell`], implemented on top of
/// [`loom::cell::UnsafeCell`][unsafecell].
///
/// Unlike [`loom::cell::UnsafeCell`][unsafecell], this provides an API that's
/// largely compatible with the standard counterpart.
///
/// [unsafecell]: crate::cell::UnsafeCell
#[derive(Debug)]
pub struct Cell<T> {
    cell: UnsafeCell<T>,
}

// unsafe impl<T> Send for Cell<T> where T: Send {}

impl<T> Cell<T> {
    /// Creates a new instance of `Cell` wrapping the given value.
    #[track_caller]
    pub fn new(v: T) -> Self {
        Self {
            cell: UnsafeCell::new(v),
        }
    }

    /// Sets the contained value.
    #[track_caller]
    pub fn set(&self, val: T) {
        // Traced as `Cell::set`, and not as the `Cell::replace` it is implemented with.
        let old = self.replace_named("Cell::set", val);
        drop(old);
    }

    /// Swaps the values of two Cells.
    #[track_caller]
    pub fn swap(&self, other: &Self) {
        if core::ptr::eq(self, other) {
            return;
        }
        // Both cells are accessed, so both are traced.
        self.cell.with_mut_named("Cell::swap", |my_ptr| {
            other.cell.with_mut_named("Cell::swap", |their_ptr| unsafe {
                core::ptr::swap(my_ptr, their_ptr);
            })
        })
    }

    /// Replaces the contained value, and returns it.
    #[track_caller]
    pub fn replace(&self, val: T) -> T {
        self.replace_named("Cell::replace", val)
    }

    /// `op` is only used for tracing, see `UnsafeCell::with_mut_named`.
    #[track_caller]
    fn replace_named(&self, op: &'static str, val: T) -> T {
        self.cell
            .with_mut_named(op, |ptr| unsafe { core::mem::replace(&mut *ptr, val) })
    }

    /// Returns a copy of the contained value.
    #[track_caller]
    pub fn get(&self) -> T
    where
        T: Copy,
    {
        self.cell.with_named("Cell::get", |ptr| unsafe { *ptr })
    }

    /// Takes the value of the cell, leaving `Default::default()` in its place.
    #[track_caller]
    pub fn take(&self) -> T
    where
        T: Default,
    {
        self.replace_named("Cell::take", T::default())
    }

    /// Unwraps the value, consuming the cell.
    #[track_caller]
    pub fn into_inner(self) -> T {
        self.cell.into_inner()
    }
}

impl<T: Default> Default for Cell<T> {
    #[track_caller]
    fn default() -> Cell<T> {
        Cell::new(T::default())
    }
}

impl<T: Copy> Clone for Cell<T> {
    #[track_caller]
    fn clone(&self) -> Cell<T> {
        Cell::new(self.get())
    }
}

impl<T> From<T> for Cell<T> {
    #[track_caller]
    fn from(src: T) -> Cell<T> {
        Cell::new(src)
    }
}

impl<T: PartialEq + Copy> PartialEq for Cell<T> {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl<T: Eq + Copy> Eq for Cell<T> {}

impl<T: PartialOrd + Copy> PartialOrd for Cell<T> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.get().partial_cmp(&other.get())
    }
}

impl<T: Ord + Copy> Ord for Cell<T> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.get().cmp(&other.get())
    }
}
