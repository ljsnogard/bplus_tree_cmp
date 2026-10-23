//! B+ Tree 的节点类型。
//!
//! 节点只保存 `SlotPtr`，不拥有实际的 `(K, V)`：
//!
//! - 叶子保存 `(SlotPtr<(K, V)>, ())`；
//! - 索引保存 `(SlotPtr<(K, V)>, TreeNodePtr)`，其中 `SlotPtr` 是该孩子子树
//!   最大 key 的引用，`TreeNodePtr` 指向孩子节点。
//!
//! 树的 root 和首尾叶由 [`crate::BPlusTree`] 自己持有。root 可以是
//! [`TreeNode::Leaf`]，也可以是 [`TreeNode::Index`]。

use crate::{ordered_arr_::OrderedArray, slab_list_::SlotPtr};

/// 叶子节点使用的有序数组，实际存储类型为 `(SlotPtr<(K, V)>, ())`。
pub(crate) type DataPtrOrdArr<const M: usize, K, V = ()> = OrderedArray<M, SlotPtr<(K, V)>, ()>;

/// 索引节点与 root 使用的有序数组，实际存储类型为
/// `(SlotPtr<(K, V)>, TreeNodePtr<M, K, V>)`。
///
/// 每个索引项的分隔 key 是孩子子树最大 key 的句柄，而不是 Key 的副本。
pub(crate) type IndexPtrOrdArr<const M: usize, K, V> =
    OrderedArray<M, SlotPtr<(K, V)>, TreeNodePtr<M, K, V>>;

/// 指向 `SlabList` 中树节点的类型安全句柄。
pub(crate) type TreeNodePtr<const M: usize, K, V = ()> = SlotPtr<TreeNode<M, K, V>>;

/// B+ Tree 节点。
#[derive(Debug)]
pub(crate) enum TreeNode<const M: usize, K, V> {
    /// 索引节点：保存 `(子树最大 key 的引用, 孩子指针)`。
    Index(IndexNode<M, K, V>),

    /// 叶子节点：保存实际数据的引用，并通过 `prev` / `next` 组成叶子链。
    Leaf(DataLeaf<M, K, V>),
}

/// 索引节点。
#[derive(Debug)]
pub(crate) struct IndexNode<const M: usize, K, V> {
    /// 按子树最大 key 排序的 `(separator, child)` 数组。
    pub(crate) children_: IndexPtrOrdArr<M, K, V>,
}

/// 叶子节点。
#[derive(Debug)]
pub(crate) struct DataLeaf<const M: usize, K, V> {
    /// 按数据 key 排序的数据引用数组。
    pub(crate) order_arr_: DataPtrOrdArr<M, K, V>,

    /// 前一个叶子；首叶为 `None`。
    pub(crate) prev_sibl_: Option<TreeNodePtr<M, K, V>>,

    /// 后一个叶子；尾叶为 `None`。
    pub(crate) next_sibl_: Option<TreeNodePtr<M, K, V>>,
}

impl<const M: usize, K, V> IndexNode<M, K, V> {
    /// 创建一个空索引节点。
    pub(crate) const fn new_() -> Self {
        IndexNode {
            children_: OrderedArray::new(),
        }
    }

    /// 返回索引项数量，也就是孩子数量。
    pub(crate) const fn len_(&self) -> usize {
        self.children_.len()
    }
}

impl<const M: usize, K, V> DataLeaf<M, K, V> {
    /// 创建一个空叶子节点。
    pub(crate) const fn new_() -> Self {
        DataLeaf {
            order_arr_: OrderedArray::new(),
            prev_sibl_: Option::None,
            next_sibl_: Option::None,
        }
    }

    /// 返回叶子中的元素数量。
    pub(crate) const fn len_(&self) -> usize {
        self.order_arr_.len()
    }
}

impl<const M: usize, K, V> TreeNode<M, K, V> {
    /// 返回节点中的项数：叶子为数据项数，索引为孩子数。
    pub(crate) fn len_(&self) -> usize {
        match self {
            TreeNode::Index(index) => index.len_(),
            TreeNode::Leaf(leaf) => leaf.len_(),
        }
    }

    /// 返回节点是否叶子。
    pub(crate) fn is_leaf_(&self) -> bool {
        matches!(self, TreeNode::Leaf(_))
    }

    /// 以索引节点引用访问；叶子返回 `None`。
    pub(crate) fn as_index_(&self) -> Option<&IndexNode<M, K, V>> {
        match self {
            TreeNode::Index(index) => Option::Some(index),
            TreeNode::Leaf(_) => Option::None,
        }
    }

    /// 以可变索引节点引用访问；叶子返回 `None`。
    pub(crate) fn as_index_mut_(&mut self) -> Option<&mut IndexNode<M, K, V>> {
        match self {
            TreeNode::Index(index) => Option::Some(index),
            TreeNode::Leaf(_) => Option::None,
        }
    }

    /// 以叶子引用访问；索引返回 `None`。
    pub(crate) fn as_leaf_(&self) -> Option<&DataLeaf<M, K, V>> {
        match self {
            TreeNode::Leaf(leaf) => Option::Some(leaf),
            TreeNode::Index(_) => Option::None,
        }
    }

    /// 以可变叶子引用访问；索引返回 `None`。
    pub(crate) fn as_leaf_mut_(&mut self) -> Option<&mut DataLeaf<M, K, V>> {
        match self {
            TreeNode::Leaf(leaf) => Option::Some(leaf),
            TreeNode::Index(_) => Option::None,
        }
    }

    /// 返回节点中逻辑上最后一个数据 key 的句柄。
    pub(crate) fn last_key_(&self) -> Option<&SlotPtr<(K, V)>> {
        match self {
            TreeNode::Index(index) => index.children_.last().map(|(key, _)| key),
            TreeNode::Leaf(leaf) => leaf.order_arr_.last().map(|(key, _)| key),
        }
    }

    /// 返回节点最大数据 key 的句柄副本。
    pub(crate) fn max_key_(&self) -> Option<SlotPtr<(K, V)>> {
        self.last_key_().copied()
    }
}
