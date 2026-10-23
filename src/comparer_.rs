use core::{cmp::Ordering, marker::PhantomData};

use crate::slab_list_::SlotPtr;

/// Comparer used by [`crate::BPlusTree`].
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

/// 比较两个指向 `(K, V)` 的 [`SlotPtr`]，只使用其中的 `K` 参与比较。
///
/// B+ 树的节点数组以 `SlotPtr<(K, V)>` 作为排序键，而用户提供的
/// `C: TrComparer<K>` 只定义 `K` 的比较关系；该适配器负责把二者连接起来。
pub(crate) struct SlotPairKeyComparer<'a, K, V, C>
where
    C: TrComparer<K>,
{
    comparer_: &'a C,
    _mark_: PhantomData<fn() -> (K, V)>,
}

impl<'a, K, V, C> SlotPairKeyComparer<'a, K, V, C>
where
    C: TrComparer<K>,
{
    pub(crate) const fn new_(comparer: &'a C) -> Self {
        SlotPairKeyComparer {
            comparer_: comparer,
            _mark_: PhantomData,
        }
    }
}

impl<K, V, C> TrComparer<SlotPtr<(K, V)>> for SlotPairKeyComparer<'_, K, V, C>
where
    C: TrComparer<K>,
{
    #[inline]
    fn compare(&self, a: &SlotPtr<(K, V)>, b: &SlotPtr<(K, V)>) -> Ordering {
        let (a_key, _) = &**a;
        let (b_key, _) = &**b;
        self.comparer_.compare(a_key, b_key)
    }
}

/// 把指向 `(K, V)` 的 [`SlotPtr`] 与异构查询对象 `Q` 比较。
///
/// 该适配器让节点数组可以直接用 `TrComparer<K, Q>` 搜索：数组元素是
/// `SlotPtr<(K, V)>`，比较时先解引用到实际数据，再只取其中的 `K`。
pub(crate) struct QuerySlotComparer<'a, K, V, Q, C>
where
    Q: ?Sized,
    C: TrComparer<K, Q>,
{
    comparer_: &'a C,
    _mark_: PhantomData<fn() -> (K, V)>,
    _query_: PhantomData<*const Q>,
}

impl<'a, K, V, Q, C> QuerySlotComparer<'a, K, V, Q, C>
where
    Q: ?Sized,
    C: TrComparer<K, Q>,
{
    pub(crate) const fn new_(comparer: &'a C) -> Self {
        QuerySlotComparer {
            comparer_: comparer,
            _mark_: PhantomData,
            _query_: PhantomData,
        }
    }
}

impl<K, V, Q, C> TrComparer<SlotPtr<(K, V)>, Q> for QuerySlotComparer<'_, K, V, Q, C>
where
    Q: ?Sized,
    C: TrComparer<K, Q>,
{
    #[inline]
    fn compare(&self, a: &SlotPtr<(K, V)>, b: &Q) -> Ordering {
        let (a_key, _) = &**a;
        self.comparer_.compare(a_key, b)
    }
}
