use core::{cmp, marker::PhantomData};

use crate::{comparer_::TrComparer, ordered_arr_::OrderedArray, slab_list_::SlotPtr};

/// OrderedArray type used in DataLeaf, the actual storage type is `(SlotPtr<(K, V)>, ())`.
///
/// This needs a comparer on `SlotPtr<(K, V)>` that dereferences the handle to the
/// `(K, V)` pair in the data SlabList and then compares the pair.
///
/// `HomoSlotComparer<'_, (K, V), C>` can be used, where `C: TrComparer<(K, V)>`
/// is the real comparer on `(K, V)`.
pub type DataPtrOrdArr<const M: usize, K, V = ()> = OrderedArray<M, SlotPtr<(K, V)>, ()>;

/// OrderedArray type used in IndexNode and RootNode, the actual storage type is
/// `(SlotPtr<(K, V)>, TreeNodePtr<M, K, V>)`.
///
/// The separator key is a handle to the actual `(K, V)` pair; it is not copied.
/// `IndexKeyComparer` / `IndexOrdKeyComparer` compare a query key against the
/// separator by dereferencing that handle.
pub type IndexPtrOrdArr<const M: usize, K, V> =
    OrderedArray<M, SlotPtr<(K, V)>, TreeNodePtr<M, K, V>>;

/// Type-safe handle to a tree node stored in a `SlabList`.
pub type TreeNodePtr<const M: usize, K, V = ()> = SlotPtr<TreeNode<M, K, V>>;

#[derive(Debug)]
pub enum TreeNode<const M: usize, K, V> {
    Root(RootNode<M, K, V>),
    Index(IndexNode<M, K, V>),
    Leaf(DataLeaf<M, K, V>),
}

#[derive(Debug)]
pub struct RootNode<const M: usize, K, V> {
    pub(crate) children_: IndexPtrOrdArr<M, K, V>,
    pub(crate) sentinel_: Option<LeafSentinel<M, K, V>>,
}

///
#[derive(Debug)]
pub struct IndexNode<const M: usize, K, V> {
    pub(crate) children_: IndexPtrOrdArr<M, K, V>,
    pub(crate) parent_: TreeNodePtr<M, K, V>,
}

#[derive(Debug)]
pub struct DataLeaf<const M: usize, K, V> {
    pub(crate) order_arr_: DataPtrOrdArr<M, K, V>,
    pub(crate) prev_sibl_: TreeNodePtr<M, K, V>,
    pub(crate) next_sibl_: TreeNodePtr<M, K, V>,
}

#[derive(Debug)]
pub struct LeafSentinel<const M: usize, K, V> {
    pub(crate) head_: TreeNodePtr<M, K, V>,
    pub(crate) tail_: TreeNodePtr<M, K, V>,
}

fn select_key<'f, K, V>(t: (&'f K, &'f V)) -> &'f K {
    t.0
}

impl<const M: usize, K, V> TreeNode<M, K, V> {
    pub fn first_key(&self) -> Option<&SlotPtr<(K, V)>> {
        match self {
            TreeNode::Leaf(leaf) => leaf.order_arr_.first().map(select_key),
            TreeNode::Index(index) => index.children_.first().map(select_key),
            TreeNode::Root(root) => root.children_.first().map(select_key),
        }
    }

    pub fn last_key(&self) -> Option<&SlotPtr<(K, V)>> {
        match self {
            TreeNode::Leaf(leaf) => leaf.order_arr_.last().map(select_key),
            TreeNode::Index(index) => index.children_.last().map(select_key),
            TreeNode::Root(root) => root.children_.last().map(select_key),
        }
    }
}

pub trait TrPickKeyPolicy<const M: usize, K, V> {
    fn pick<'f>(node: &'f TreeNode<M, K, V>) -> &'f K;
}

pub struct PickLastKey<const M: usize, K, V>(PhantomData<fn() -> (K, V)>);
pub struct PickFirstKey<const M: usize, K, V>(PhantomData<fn() -> (K, V)>);

impl<const M: usize, K, V> TrPickKeyPolicy<M, K, V> for PickLastKey<M, K, V> {
    fn pick<'f>(node: &'f TreeNode<M, K, V>) -> &'f K {
        let Option::Some(k) = node.last_key() else {
            unreachable!("empty node encountered")
        };
        let (k, _) = &**k;
        k
    }
}

impl<const M: usize, K, V> TrPickKeyPolicy<M, K, V> for PickFirstKey<M, K, V> {
    fn pick<'f>(node: &'f TreeNode<M, K, V>) -> &'f K {
        let Option::Some(k) = node.first_key() else {
            unreachable!("empty node encountered")
        };
        let (k, _) = &**k;
        k
    }
}

pub struct IndexKeyComparer<'a, const M: usize, P, C, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    C: TrComparer<K>,
{
    k_cmp_: &'a C,
    _mark_: PhantomData<fn() -> (P, K, V)>,
}

impl<'a, const M: usize, P, C, K, V> IndexKeyComparer<'a, M, P, C, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    C: TrComparer<K>,
{
    const fn new(key_cmp: &'a C) -> Self {
        IndexKeyComparer {
            k_cmp_: key_cmp,
            _mark_: PhantomData,
        }
    }

    pub fn compare(&self, lhs: &(K, V), rhs: &TreeNode<M, K, V>) -> cmp::Ordering {
        let (l_key, _) = lhs;
        let r_key = <P as TrPickKeyPolicy<M, K, V>>::pick(rhs);
        self.k_cmp_.compare(l_key, r_key)
    }
}

impl<'a, const M: usize, C, K, V> IndexKeyComparer<'a, M, PickFirstKey<M, K, V>, C, K, V>
where
    C: TrComparer<K>,
    K: Ord,
{
    pub const fn pick_first_key(key_cmp: &'a C) -> Self {
        Self::new(key_cmp)
    }
}

impl<'a, const M: usize, C, K, V> IndexKeyComparer<'a, M, PickLastKey<M, K, V>, C, K, V>
where
    C: TrComparer<K>,
    K: Ord,
{
    pub const fn pick_last_key(key_cmp: &'a C) -> Self {
        Self::new(key_cmp)
    }
}

impl<const M: usize, P, C, K, V> TrComparer<(K, V), TreeNode<M, K, V>>
    for IndexKeyComparer<'_, M, P, C, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    C: TrComparer<K>,
{
    #[inline]
    fn compare(&self, a: &(K, V), b: &TreeNode<M, K, V>) -> cmp::Ordering {
        IndexKeyComparer::compare(self, a, b)
    }
}

pub struct IndexOrdKeyComparer<const M: usize, P, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    K: Ord,
{
    _mark_: PhantomData<fn() -> (P, K, V)>,
}

impl<const M: usize, P, K, V> IndexOrdKeyComparer<M, P, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    K: Ord,
{
    const fn new() -> Self {
        IndexOrdKeyComparer {
            _mark_: PhantomData,
        }
    }

    pub fn compare(&self, lhs: &(K, V), rhs: &TreeNode<M, K, V>) -> cmp::Ordering {
        let (l_key, _) = lhs;
        let r_key = <P as TrPickKeyPolicy<M, K, V>>::pick(rhs);
        <K as Ord>::cmp(l_key, r_key)
    }
}

impl<const M: usize, K, V> IndexOrdKeyComparer<M, PickFirstKey<M, K, V>, K, V>
where
    K: Ord,
{
    pub const fn pick_first_key() -> Self {
        Self::new()
    }
}

impl<const M: usize, K, V> IndexOrdKeyComparer<M, PickLastKey<M, K, V>, K, V>
where
    K: Ord,
{
    pub const fn pick_last_key() -> Self {
        Self::new()
    }
}

impl<const M: usize, P, K, V> TrComparer<(K, V), TreeNode<M, K, V>>
    for IndexOrdKeyComparer<M, P, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    K: Ord,
{
    #[inline]
    fn compare(&self, a: &(K, V), b: &TreeNode<M, K, V>) -> cmp::Ordering {
        IndexOrdKeyComparer::compare(self, a, b)
    }
}
