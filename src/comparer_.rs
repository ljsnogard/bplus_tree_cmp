use core::{
    cmp::Ordering,
    marker::PhantomData,
};

/// Comparer used by [`BPlusTree`].
///
/// The map does not impose any trait bound on `K`. The comparer is the complete
/// definition of key ordering.
pub trait TrComparer<A, B = A>
where
    A: ?Sized,
    B: ?Sized,
{
    fn compare(&self, a: &A, b: &B) -> Ordering;
}

/// Built-in comparator which delegates to `Ord`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OrdComparer<T>(PhantomData<T>)
where
    T: ?Sized;

impl<T> OrdComparer<T>
where
    T: ?Sized + Ord,
{
    pub const fn new() -> Self {
        OrdComparer(PhantomData)
    }
}

impl<T> TrComparer<T, T> for OrdComparer<T>
where
    T: ?Sized + Ord,
{
    #[inline]
    fn compare(&self, a: &T, b: &T) -> Ordering {
        <T as Ord>::cmp(a, b)
    }
}

impl<A, B, F> TrComparer<A, B> for F
where
    A: ?Sized,
    B: ?Sized,
    F: Fn(&A, &B) -> Ordering,
{
    #[inline]
    fn compare(&self, a: &A, b: &B) -> Ordering {
        let f = self;
        f(a, b)
    }
}
