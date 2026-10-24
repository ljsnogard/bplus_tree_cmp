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

/// 通过 `SlotPtr` 取得 B+ Tree 节点引用。
///
/// # 安全性
///
/// - `ptr` 必须指向仍然存活、且属于当前树的节点；
/// - 在返回引用的生命周期内，不能存在指向同一节点的可变引用。
pub(crate) unsafe fn node_ref_<'a, const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
) -> &'a TreeNode<M, K, V> {
    // SAFETY: 调用方保证 ptr 指向存活节点。
    unsafe { &*ptr.as_ptr() }
}

/// 通过 `SlotPtr` 取得 B+ Tree 节点可变引用。
///
/// # 安全性
///
/// - `ptr` 必须指向仍然存活、且属于当前树的节点；
/// - 在返回引用的生命周期内，不能存在指向同一节点的其他引用。
pub(crate) unsafe fn node_mut_<'a, const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
) -> &'a mut TreeNode<M, K, V> {
    // SAFETY: 调用方保证 ptr 指向存活节点且没有别名。
    unsafe { &mut *ptr.as_ptr() }
}

/// 所有 B+ Tree 节点共享的元数据。
#[derive(Debug)]
pub(crate) struct NodeBase<const M: usize, K, V> {
    /// 父节点；根节点为 `None`。
    pub(crate) parent_: Option<TreeNodePtr<M, K, V>>,
}

impl<const M: usize, K, V> NodeBase<M, K, V> {
    /// 创建一个 `parent_` 为 `None` 的节点元数据。
    pub(crate) const fn new_() -> Self {
        NodeBase {
            parent_: Option::None,
        }
    }
}

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
    /// 节点共享元数据。
    pub(crate) base_: NodeBase<M, K, V>,

    /// 按子树最大 key 排序的 `(separator, child)` 数组。
    pub(crate) children_: IndexPtrOrdArr<M, K, V>,
}

/// 叶子节点。
#[derive(Debug)]
pub(crate) struct DataLeaf<const M: usize, K, V> {
    /// 节点共享元数据。
    pub(crate) base_: NodeBase<M, K, V>,

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
            base_: NodeBase::new_(),
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
            base_: NodeBase::new_(),
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

    /// 返回父节点。
    pub(crate) fn parent_(&self) -> Option<TreeNodePtr<M, K, V>> {
        match self {
            TreeNode::Index(index) => index.base_.parent_,
            TreeNode::Leaf(leaf) => leaf.base_.parent_,
        }
    }

    /// 设置父节点。
    pub(crate) fn set_parent_(&mut self, parent: Option<TreeNodePtr<M, K, V>>) {
        match self {
            TreeNode::Index(index) => index.base_.parent_ = parent,
            TreeNode::Leaf(leaf) => leaf.base_.parent_ = parent,
        }
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

/// 返回节点项数。
pub(crate) fn node_len_<const M: usize, K, V>(ptr: TreeNodePtr<M, K, V>) -> usize {
    // SAFETY: 调用方传入的节点指针由树维护。
    unsafe { node_ref_(ptr) }.len_()
}

/// 返回节点父指针。
pub(crate) fn node_parent_<const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
) -> Option<TreeNodePtr<M, K, V>> {
    // SAFETY: 调用方传入的节点指针由树维护。
    unsafe { node_ref_(ptr) }.parent_()
}

/// 设置节点父指针。
pub(crate) fn node_set_parent_<const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
    parent: Option<TreeNodePtr<M, K, V>>,
) {
    // SAFETY: 调用方保证在本次写入期间没有其他引用访问该节点。
    unsafe { node_mut_(ptr) }.set_parent_(parent);
}

/// 返回 `child` 在 `parent` 中的孩子下标。
pub(crate) fn child_index_<const M: usize, K, V>(
    parent: TreeNodePtr<M, K, V>,
    child: TreeNodePtr<M, K, V>,
) -> Option<usize> {
    // SAFETY: 调用方保证 parent/child 由树维护。
    let node = unsafe { node_ref_(parent) };
    let index = node.as_index_()?;

    for idx in 0..index.len_() {
        if let Ok((_, child_ptr)) = index.children_.get_ref_at(idx)
            && *child_ptr == child
        {
            return Option::Some(idx);
        }
    }
    Option::None
}

/// 把 `parent` 的所有孩子的父指针设置为 `parent`。
pub(crate) fn reparent_children_<const M: usize, K, V>(parent: TreeNodePtr<M, K, V>) {
    // SAFETY: 调用方保证 parent 及其孩子由树维护；孩子与 parent 不重叠。
    let node = unsafe { node_ref_(parent) };
    let Option::Some(index) = node.as_index_() else {
        return;
    };

    for idx in 0..index.len_() {
        if let Ok((_, child_ptr)) = index.children_.get_ref_at(idx) {
            node_set_parent_(*child_ptr, Option::Some(parent));
        }
    }
}

/// 返回节点是否为叶子。
pub(crate) fn node_is_leaf_<const M: usize, K, V>(ptr: TreeNodePtr<M, K, V>) -> bool {
    // SAFETY: 调用方传入的节点指针由树维护。
    unsafe { node_ref_(ptr) }.is_leaf_()
}

/// 返回节点最大数据 key 的句柄。
pub(crate) fn node_max_<const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
) -> Option<SlotPtr<(K, V)>> {
    // SAFETY: 调用方传入的节点指针由树维护。
    unsafe { node_ref_(ptr) }.max_key_()
}

/// 返回索引节点第 `idx` 个孩子。
pub(crate) fn child_at_<const M: usize, K, V>(
    parent: TreeNodePtr<M, K, V>,
    idx: usize,
) -> Option<TreeNodePtr<M, K, V>> {
    // SAFETY: 调用方传入的节点指针由树维护。
    let node = unsafe { node_ref_(parent) };
    let index = node.as_index_()?;
    let (_, child) = index.children_.get_ref_at(idx).ok()?;
    Option::Some(*child)
}
