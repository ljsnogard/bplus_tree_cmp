use core::cmp;
use crate::{
    arena_::{Arena, Idx},
    comparer_::TrComparer,
    ordered_arr_::OrderedArray,
};

pub type OrdDataIdArr<T, const M: usize> = OrderedArray<Idx<T>, M>;
pub type TreeNodeId<T, const M: usize> = Idx<TreeNode<T, M>>;

#[derive(Debug)]
pub enum TreeNode<T, const M: usize> {
    Root(RootNode<T, M>),
    Index(IndexNode<T, M>),
    Leaf(DataLeaf<T, M>),
}

#[derive(Debug)]
pub struct RootNode<T, const M: usize> {
    pub(crate)children_: OrderedArray<TreeNodeId<T, M>, M>,
    pub(crate)sentinel_: Option<LeafSentinel<T, M>>,
}

#[derive(Debug)]
pub struct IndexNode<T, const M: usize> {
    pub(crate)children_: OrderedArray<TreeNodeId<T, M>, M>,
    pub(crate)parent_: TreeNodeId<T, M>,
}

#[derive(Debug)]
pub struct DataLeaf<T, const M: usize> {
    pub(crate)order_arr_: OrdDataIdArr<T, M>,
    pub(crate)prev_sibl_: TreeNodeId<T, M>,
    pub(crate)next_sibl_: TreeNodeId<T, M>,
}

#[derive(Debug)]
pub struct LeafSentinel<T, const M: usize> {
    pub(crate)head_: TreeNodeId<T, M>,
    pub(crate)tail_: TreeNodeId<T, M>,
}

