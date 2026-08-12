use core::{cmp, marker::PhantomData};

use crate::{
    arena_::{self, Arena, Idx}, comparer_::TrComparer, ordered_arr_::OrderedArray,
};

/// OrderedArray type used in DataLeaf, the actual storage type is `(Idx<K, V>, ())`.
///
/// So this will need `HomoIdxComparer<'_, (K, V), TyPc>`, to map the compare operation into the arena of (K, V).
///
/// Here `TyPc = PairAdaptComparer<'_, K, C>`, to map the compare operation onto the K in (K, V).
///
/// And then `C: TrComparer<K>` is the real comparer on K.
///
/// However, if `K: Ord` is satisfied, this could be mush easier with a customized comparer to replace
/// `PairAdaptComparer`.
pub type DataIdOrdArr<const M: usize, K, V = ()> = OrderedArray<M, Idx<(K, V)>, ()>;

/// OrderedArray type used in IndexNode and RootNode, the actual storage type is `(Idx<K, V>, TreeNodeId<M, K, V>)`.
///
/// So this will need `HeteroIdxComparer<'_, (K, V), (K, V), TyPc>`, to map the compare operation into the arena of
/// (K, V).
///
/// Here `TyPc = PairAdaptComparer<'_, K, C>`, to map the compare operation onto the K in (K, V).
/// Just like in `DataIdOrdArr`.
///
/// And then `C: TrCompare<Idx<(K, V)>, Idx<TreeNode<M, K, V>>>` is the real comparer on K.
///
/// You can choose `IndexOrdKeyComparer` if `K: Ord`.
///
/// Otherwise, choose `IndexOrdKeyComparer` and provide comparer for `K` in its generic type param.
pub type IndexIdOrdArr<const M: usize, K, V> = OrderedArray<M, Idx<(K, V)>, TreeNodeId<M, K, V>>;

pub type TreeNodeId<const M: usize, K, V = ()> = Idx<TreeNode<M, K, V>>;

#[derive(Debug)]
pub enum TreeNode<const M: usize, K, V> {
    Root(RootNode<M, K, V>),
    Index(IndexNode<M, K, V>),
    Leaf(DataLeaf<M, K, V>),
}

#[derive(Debug)]
pub struct RootNode<const M: usize, K, V> {
    pub(crate)children_: IndexIdOrdArr<M, K, V>,
    pub(crate)sentinel_: Option<LeafSentinel<M, K, V>>,
}

///
#[derive(Debug)]
pub struct IndexNode<const M: usize, K, V> {
    pub(crate)children_: IndexIdOrdArr<M, K, V>,
    pub(crate)parent_: TreeNodeId<M, K, V>,
}

#[derive(Debug)]
pub struct DataLeaf<const M: usize, K, V> {
    pub(crate)order_arr_: DataIdOrdArr<M, K, V>,
    pub(crate)prev_sibl_: TreeNodeId<M, K, V>,
    pub(crate)next_sibl_: TreeNodeId<M, K, V>,
}

#[derive(Debug)]
pub struct LeafSentinel<const M: usize, K, V> {
    pub(crate)head_: TreeNodeId<M, K, V>,
    pub(crate)tail_: TreeNodeId<M, K, V>,
}

fn select_key<'f, K, V>(t: (&'f K, &'f V)) -> &'f K {
    t.0
}

impl<const M: usize, K, V> TreeNode<M, K, V> {
    pub fn first_key(&self) -> Option<&Idx<(K, V)>> {
        match self {
            TreeNode::Leaf(leaf) => leaf.order_arr_.first().map(select_key),
            TreeNode::Index(index) => index.children_.first().map(select_key),
            TreeNode::Root(root) => root.children_.first().map(select_key),
        }
    }

    pub fn last_key(&self) -> Option<&Idx<(K, V)>> {
        match self {
            TreeNode::Leaf(leaf) => leaf.order_arr_.last().map(select_key),
            TreeNode::Index(index) => index.children_.last().map(select_key),
            TreeNode::Root(root) => root.children_.last().map(select_key),
        }
    }
}

pub trait TrPickKeyPolicy<const M: usize, K, V> {
    fn pick<'f>(node: &'f TreeNode<M, K, V>, arena: &'f Arena<(K, V)>) -> &'f K;
}

pub struct PickLastKey<const M: usize, K, V>(PhantomData<fn() -> (K, V)>);
pub struct PickFirstKey<const M: usize, K, V>(PhantomData<fn() -> (K, V)>);

impl<const M: usize, K, V> TrPickKeyPolicy<M, K, V> for PickLastKey<M, K, V> {
    fn pick<'f>(node: &'f TreeNode<M, K, V>, arena: &'f Arena<(K, V)>) -> &'f K {
        let Option::Some(k) = node.last_key() else {
            unreachable!("empty node encountered")
        };
        let (k, _) = k.get(arena);
        k
    }
}

impl<const M: usize, K, V> TrPickKeyPolicy<M, K, V> for PickFirstKey<M, K, V> {
    fn pick<'f>(node: &'f TreeNode<M, K, V>, arena: &'f Arena<(K, V)>) -> &'f K {
        let Option::Some(k) = node.first_key() else {
            unreachable!("empty node encountered")
        };
        let (k, _) = k.get(arena);
        k
    }
}

pub struct IndexKeyComparer<'a, const M: usize, P, C, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    C: TrComparer<K>,
{
    k_cmp_: &'a C,
    arena_: &'a Arena<(K, V)>,
    _mark_: PhantomData<fn() -> P>,
}

impl<'a, const M: usize, P, C, K, V> IndexKeyComparer<'a, M, P, C, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    C: TrComparer<K>,
{
    const fn new(key_cmp: &'a C, arena: &'a Arena<(K, V)>) -> Self {
        IndexKeyComparer {
            k_cmp_: key_cmp,
            arena_: arena,
            _mark_: PhantomData,
        }
    }

    pub fn compare(
        &self,
        lhs: &(K, V),
        rhs: &TreeNode<M, K, V>,
    ) -> cmp::Ordering {
        let (l_key, _) = lhs;
        let r_key = <P as TrPickKeyPolicy<M, K, V>>::pick(rhs, self.arena_);
        self.k_cmp_.compare(l_key, r_key)
    }
}

impl<'a, const M: usize, C, K, V> IndexKeyComparer<'a, M, PickFirstKey<M, K, V>, C, K, V>
where
    C: TrComparer<K>,
    K: Ord,
{
    pub const fn pick_first_key(key_cmp: &'a C, arena: &'a Arena<(K, V)>) -> Self {
        Self::new(key_cmp, arena)
    }
}

impl<'a, const M: usize, C, K, V> IndexKeyComparer<'a, M, PickLastKey<M, K, V>, C, K, V>
where
    C: TrComparer<K>,
    K: Ord,
{
    pub const fn pick_last_key(key_cmp: &'a C, arena: &'a Arena<(K, V)>) -> Self {
        Self::new(key_cmp, arena)
    }
}

impl<const M: usize, P, C, K, V> TrComparer<(K, V), TreeNode<M, K, V>> for IndexKeyComparer<'_, M, P, C, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    C: TrComparer<K>,
{
    #[inline]
    fn compare(&self, a: &(K, V), b: &TreeNode<M, K, V>) -> cmp::Ordering {
        IndexKeyComparer::compare(self, a, b)
    }
}

pub struct IndexOrdKeyComparer<'a, const M: usize, P, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    K: Ord,
{
    arena_: &'a Arena<(K, V)>,
    _mark_: PhantomData<fn() -> P>,
}

impl<'a, const M: usize, P, K, V> IndexOrdKeyComparer<'a, M, P, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    K: Ord,
{
    const fn new(arena: &'a Arena<(K, V)>) -> Self {
        IndexOrdKeyComparer {
            arena_: arena,
            _mark_: PhantomData,
        }
    }

    pub fn compare(
        &self,
        lhs: &(K, V),
        rhs: &TreeNode<M, K, V>,
    ) -> cmp::Ordering {
        let (l_key, _) = lhs;
        let r_key = <P as TrPickKeyPolicy<M, K, V>>::pick(rhs, self.arena_);
        <K as Ord>::cmp(l_key, r_key)
    }
}

impl<'a, const M: usize, K, V> IndexOrdKeyComparer<'a, M, PickFirstKey<M, K, V>, K, V>
where
    K: Ord,
{
    pub const fn pick_first_key(arena: &'a Arena<(K, V)>) -> Self {
        Self::new(arena)
    }
}

impl<'a, const M: usize, K, V> IndexOrdKeyComparer<'a, M, PickLastKey<M, K, V>, K, V>
where
    K: Ord,
{
    pub const fn pick_last_key(arena: &'a Arena<(K, V)>) -> Self {
        Self::new(arena)
    }
}

impl<const M: usize, P, K, V> TrComparer<(K, V), TreeNode<M, K, V>> for IndexOrdKeyComparer<'_, M, P, K, V>
where
    P: TrPickKeyPolicy<M, K, V>,
    K: Ord,
{
    #[inline]
    fn compare(&self, a: &(K, V), b: &TreeNode<M, K, V>) -> cmp::Ordering {
        IndexOrdKeyComparer::compare(self, a, b)
    }
}
