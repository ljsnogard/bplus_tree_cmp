use core::{cmp::Ordering, marker::PhantomData};

use crate::slab_list_::SlotPtr;

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

impl<T> TrComparer<T> for OrdComparer<T>
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

pub struct PairAdaptComparer<'a, T, C>
where
    C: TrComparer<T>,
{
    t_cmp_: &'a C,
    _t_: PhantomData<fn(&'a T) -> Ordering>,
}

impl<'a, T, C> PairAdaptComparer<'a, T, C>
where
    C: TrComparer<T>,
{
    pub const fn new(cmp: &'a C) -> Self {
        PairAdaptComparer {
            t_cmp_: cmp,
            _t_: PhantomData,
        }
    }
}

impl<'a, T, C> TrComparer<(T, ())> for PairAdaptComparer<'a, T, C>
where
    C: TrComparer<T>,
{
    #[inline]
    fn compare(&self, a: &(T, ()), b: &(T, ())) -> Ordering {
        self.t_cmp_.compare(&a.0, &b.0)
    }
}

/// 同构定序：比较两个指向同类型元素的 [`SlotPtr`]。
///
/// `SlotPtr` 直接指向元素，因此不需要再持有额外的容器引用。
pub struct HomoSlotComparer<'a, T, C>
where
    C: TrComparer<T>,
{
    comparer_: &'a C,
    _marker_: PhantomData<fn(&'a T) -> Ordering>,
}

impl<'a, T, C> HomoSlotComparer<'a, T, C>
where
    C: TrComparer<T>,
{
    pub const fn new(comparer: &'a C) -> Self {
        HomoSlotComparer {
            comparer_: comparer,
            _marker_: PhantomData,
        }
    }
}

impl<'a, T, C> TrComparer<SlotPtr<T>> for HomoSlotComparer<'a, T, C>
where
    C: TrComparer<T>,
{
    #[inline]
    fn compare(&self, a: &SlotPtr<T>, b: &SlotPtr<T>) -> Ordering {
        self.comparer_.compare(&**a, &**b)
    }
}

/// 异构定序：比较两个指向不同类型元素的 [`SlotPtr`]。
///
/// `SlotPtr` 直接指向元素，因此不需要再持有额外的容器引用。
pub struct HeteroSlotComparer<'a, L, R, C>
where
    C: TrComparer<L, R>,
{
    comparer_: &'a C,
    _marker_: PhantomData<fn(&'a L, &'a R) -> Ordering>,
}

impl<'a, L, R, C> HeteroSlotComparer<'a, L, R, C>
where
    C: TrComparer<L, R>,
{
    pub const fn new(comparer: &'a C) -> Self {
        HeteroSlotComparer {
            comparer_: comparer,
            _marker_: PhantomData,
        }
    }
}

impl<'a, L, R, C> TrComparer<SlotPtr<L>, SlotPtr<R>> for HeteroSlotComparer<'a, L, R, C>
where
    C: TrComparer<L, R>,
{
    #[inline]
    fn compare(&self, a: &SlotPtr<L>, b: &SlotPtr<R>) -> Ordering {
        self.comparer_.compare(&**a, &**b)
    }
}
