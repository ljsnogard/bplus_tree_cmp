use core::cmp;
use crate::{
    arena_::{Arena, Idx},
    comparer_::TrComparer,
    ordered_arr_::OrderedArray,
};

pub type DataIdOrdArr<const M: usize, K, V = ()> = OrderedArray<M, Idx<(K, V)>, ()>;

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
    pub(crate)children_: OrderedArray<M, TreeNodeId<M, K, V>, ()>,
    pub(crate)sentinel_: Option<LeafSentinel<M, K, V>>,
}

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

