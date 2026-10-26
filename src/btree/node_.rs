//! B 树节点。
//!
//! 与 B+ Tree 只在叶子保存数据不同，B Tree 的每个节点都可以保存实际数据。
//! 但节点仍然不复制 `K`：每个 `(K, V)` 只存在于某一个节点中，节点之间通过
//! `SlotPtr` 组成的树结构连接。
//!
//! 节点采用统一布局：
//!
//! - `entries_` 是 `OrderedArray<M, K, NodeValue>`，按 `K` 排序；
//! - `NodeValue::right_` 保存该项的右孩子；
//! - `first_child_` 为 `Some` 时表示内部节点，此时第 `i` 个项的右孩子是第
//!   `i + 1` 个孩子，`first_child_` 是第 0 个孩子；
//! - `first_child_` 为 `None` 时表示叶子节点，所有项的右孩子也都是 `None`。
//!
//! 这样内部节点最多保存 `M` 个数据项和 `M + 1` 个孩子，而不需要额外引入
//! `M + 1` 容量的数组（Rust 稳定版尚不支持在数组长度中使用泛型常量表达式）。

use core::ptr;

use crate::{ordered_arr_::OrderedArray, slab_list_::SlotPtr};

/// 指向 B 树节点的类型安全句柄。
pub(crate) type TreeNodePtr<const M: usize, K, V> = SlotPtr<Node<M, K, V>>;

/// 节点内位置：节点句柄与逻辑下标。
pub(crate) type NodePosition<const M: usize, K, V = ()> = (TreeNodePtr<M, K, V>, usize);

/// 一个范围的首、尾位置。
pub(crate) type RangePositions<const M: usize, K, V = ()> =
    (NodePosition<M, K, V>, NodePosition<M, K, V>);

/// 通过 `SlotPtr` 取得 B Tree 节点引用。
///
/// # 安全性
///
/// - `ptr` 必须指向仍然存活、且属于当前树的节点；
/// - 在返回引用的生命周期内，不能存在指向同一节点的可变引用。
pub(crate) unsafe fn node_ref_<'a, const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
) -> &'a Node<M, K, V> {
    // SAFETY: 调用方保证 ptr 指向存活节点。
    unsafe { &*ptr.as_ptr() }
}

/// 通过 `SlotPtr` 取得 B Tree 节点可变引用。
///
/// # 安全性
///
/// - `ptr` 必须指向仍然存活、且属于当前树的节点；
/// - 在返回引用的生命周期内，不能存在指向同一节点的其他引用。
pub(crate) unsafe fn node_mut_<'a, const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
) -> &'a mut Node<M, K, V> {
    // SAFETY: 调用方保证 ptr 指向存活节点且没有别名。
    unsafe { &mut *ptr.as_ptr() }
}

/// 节点中与 key 一起保存的数据。
///
/// 叶子节点的 `right_` 为 `None`；内部节点的每个 `right_` 都必须是 `Some`。
pub(crate) struct NodeValue<const M: usize, K, V> {
    /// 实际 value。
    pub(crate) value_: V,

    /// 该项的右孩子；叶子节点为 `None`。
    pub(crate) right_: Option<TreeNodePtr<M, K, V>>,
}

impl<const M: usize, K, V> NodeValue<M, K, V> {
    /// 创建一个节点值。
    pub(crate) const fn new_(value: V, right: Option<TreeNodePtr<M, K, V>>) -> Self {
        NodeValue {
            value_: value,
            right_: right,
        }
    }
}

/// 节点数组中的一个完整项：`(K, NodeValue)`。
pub(crate) type NodeEntry<const M: usize, K, V> = (K, NodeValue<M, K, V>);

/// 节点内部的有序数组。
pub(crate) type NodeArray<const M: usize, K, V> = OrderedArray<M, K, NodeValue<M, K, V>>;

/// B 树节点。
pub(crate) struct Node<const M: usize, K, V> {
    /// 父节点；根节点为 `None`。
    pub(crate) parent_: Option<TreeNodePtr<M, K, V>>,

    /// 第 0 个孩子；叶子节点为 `None`。
    pub(crate) first_child_: Option<TreeNodePtr<M, K, V>>,

    /// 按 key 排序的数据项。
    pub(crate) entries_: NodeArray<M, K, V>,
}

impl<const M: usize, K, V> Node<M, K, V> {
    /// 创建一个空叶子节点。
    pub(crate) const fn new_leaf_() -> Self {
        Node {
            parent_: Option::None,
            first_child_: Option::None,
            entries_: OrderedArray::new(),
        }
    }

    /// 创建一个只包含一个孩子的内部节点。
    pub(crate) const fn new_index_(first_child: TreeNodePtr<M, K, V>) -> Self {
        Node {
            parent_: Option::None,
            first_child_: Option::Some(first_child),
            entries_: OrderedArray::new(),
        }
    }

    /// 返回父节点。
    pub(crate) const fn parent_(&self) -> Option<TreeNodePtr<M, K, V>> {
        self.parent_
    }

    /// 设置父节点。
    pub(crate) fn set_parent_(&mut self, parent: Option<TreeNodePtr<M, K, V>>) {
        self.parent_ = parent;
    }

    /// 返回节点是否为叶子。
    pub(crate) const fn is_leaf_(&self) -> bool {
        self.first_child_.is_none()
    }

    /// 返回节点保存的项数。
    pub(crate) const fn len_(&self) -> usize {
        self.entries_.len()
    }

    /// 返回第 `idx` 个孩子。
    ///
    /// 叶子节点没有孩子，因此总是返回 `None`。内部节点满足
    /// `0 <= idx <= len_()`。
    pub(crate) fn child_at_(&self, idx: usize) -> Option<TreeNodePtr<M, K, V>> {
        if idx == 0 {
            self.first_child_
        } else {
            match self.entries_.get_ref_at(idx - 1) {
                Ok((_, node_value)) => node_value.right_,
                Err(_) => Option::None,
            }
        }
    }

    /// 返回最后一个孩子。
    pub(crate) fn last_child_(&self) -> Option<TreeNodePtr<M, K, V>> {
        match self.entries_.last() {
            Option::Some((_, node_value)) => node_value.right_,
            Option::None => self.first_child_,
        }
    }
}

/// 非根节点的最小项数。
///
/// 内部节点最多有 `M + 1` 个孩子，B 树要求非根节点的孩子数不少于
/// `ceil((M + 1) / 2)`，因此最小项数为 `ceil((M + 1) / 2) - 1`。
#[inline]
pub(crate) const fn min_keys_(degree: usize) -> usize {
    (degree + 1).div_ceil(2) - 1
}

/// 分裂一个“已满节点 + 一个待插入项”时，中间项在合并序列中的下标。
#[inline]
pub(crate) const fn split_mid_(degree: usize) -> usize {
    degree.div_ceil(2)
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
    (0..=node.len_()).find(|&idx| node.child_at_(idx) == Option::Some(child))
}

/// 把 `parent` 的所有孩子的父指针设置为 `parent`。
pub(crate) fn reparent_children_<const M: usize, K, V>(parent: TreeNodePtr<M, K, V>) {
    // SAFETY: 调用方保证 parent 及其孩子由树维护；孩子与 parent 不重叠。
    let node = unsafe { node_ref_(parent) };

    if let Option::Some(first) = node.first_child_ {
        node_set_parent_(first, Option::Some(parent));
    }

    for idx in 0..node.len_() {
        if let Ok((_, node_value)) = node.entries_.get_ref_at(idx)
            && let Option::Some(child) = node_value.right_
        {
            node_set_parent_(child, Option::Some(parent));
        }
    }
}

/// 返回节点是否为叶子。
pub(crate) fn node_is_leaf_<const M: usize, K, V>(ptr: TreeNodePtr<M, K, V>) -> bool {
    // SAFETY: 调用方传入的节点指针由树维护。
    unsafe { node_ref_(ptr) }.is_leaf_()
}

/// 返回节点第 `idx` 个孩子。
pub(crate) fn node_child_at_<const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> Option<TreeNodePtr<M, K, V>> {
    // SAFETY: 调用方传入的节点指针由树维护。
    unsafe { node_ref_(ptr) }.child_at_(idx)
}

/// 返回节点第一个孩子。
pub(crate) fn node_first_child_<const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
) -> Option<TreeNodePtr<M, K, V>> {
    // SAFETY: 调用方传入的节点指针由树维护。
    unsafe { node_ref_(ptr) }.first_child_
}

/// 设置节点第一个孩子。
pub(crate) fn node_set_first_child_<const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
    child: Option<TreeNodePtr<M, K, V>>,
) {
    // SAFETY: 调用方保证在本次写入期间没有其他引用访问该节点。
    unsafe { node_mut_(ptr) }.first_child_ = child;
}

/// 取得节点第 `idx` 项的 key 与 value 引用。
///
/// # 安全性
///
/// `ptr` 必须指向存活节点，`idx < node_len_(ptr)`，且返回引用的生命周期内
/// 不能对同一节点进行可变访问。
pub(crate) unsafe fn entry_ref_<'a, const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> (&'a K, &'a V) {
    // SAFETY: 调用方保证 ptr 与 idx 有效，且期间没有其他可变引用。
    let node = unsafe { node_ref_(ptr) };
    match node.entries_.get_ref_at(idx) {
        Ok((key, node_value)) => (key, &node_value.value_),
        Err(_) => unreachable!("B 树节点下标应有效"),
    }
}

/// 取得节点第 `idx` 项的 key 与可变 value 引用。
///
/// # 安全性
///
/// `ptr` 必须指向存活节点，`idx < node_len_(ptr)`，且调用方必须拥有对该节点的
/// 独占访问权；返回引用的生命周期内不能对同一节点进行其他访问。
pub(crate) unsafe fn entry_mut_<'a, const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> (&'a K, &'a mut V) {
    // SAFETY: 调用方保证 ptr 与 idx 有效，且拥有独占访问权。
    let node = unsafe { node_mut_(ptr) };
    match node.entries_.get_mut_at(idx) {
        Ok((key, node_value)) => (key, &mut node_value.value_),
        Err(_) => unreachable!("B 树节点下标应有效"),
    }
}

/// 通过裸指针判断节点是否为叶子。
///
/// # 安全性
///
/// `ptr` 必须指向当前树中仍然存活的节点。
pub(crate) unsafe fn node_is_leaf_raw_<const M: usize, K, V>(ptr: TreeNodePtr<M, K, V>) -> bool {
    let node = ptr.as_ptr();
    // SAFETY: 只读取 first_child_ 字段，不创建指向整个节点的引用。
    unsafe { ptr::addr_of!((*node).first_child_).read().is_none() }
}

/// 通过裸指针读取节点项数。
///
/// # 安全性
///
/// `ptr` 必须指向当前树中仍然存活的节点。
pub(crate) unsafe fn node_len_raw_<const M: usize, K, V>(ptr: TreeNodePtr<M, K, V>) -> usize {
    let node = ptr.as_ptr();
    // SAFETY: 只读取 entries_ 的计数字段，不创建指向整个节点的引用。
    let entries = unsafe { ptr::addr_of!((*node).entries_) };
    // SAFETY: entries 属于仍然存活的节点。
    unsafe { OrderedArray::len_raw_(entries) }
}

/// 通过裸指针读取父节点。
///
/// # 安全性
///
/// `ptr` 必须指向当前树中仍然存活的节点。
pub(crate) unsafe fn node_parent_raw_<const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
) -> Option<TreeNodePtr<M, K, V>> {
    let node = ptr.as_ptr();
    // SAFETY: 只读取 parent_ 字段，不创建指向整个节点的引用。
    unsafe { ptr::addr_of!((*node).parent_).read() }
}

/// 通过裸指针读取节点第 `idx` 个孩子。
///
/// # 安全性
///
/// `ptr` 必须指向当前树中仍然存活的节点，且 `idx <= node_len_raw_(ptr)`。
pub(crate) unsafe fn child_at_raw_<const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> Option<TreeNodePtr<M, K, V>> {
    let node = ptr.as_ptr();
    if idx == 0 {
        // SAFETY: 只读取 first_child_ 字段。
        return unsafe { ptr::addr_of!((*node).first_child_).read() };
    }

    // SAFETY: 只读取 entries_ 中第 idx - 1 项，不创建数组引用。
    let entries = unsafe { ptr::addr_of!((*node).entries_) };
    // SAFETY: idx - 1 由调用方保证有效。
    let elem = unsafe { OrderedArray::elem_raw_at_(entries, idx - 1) }?;
    // SAFETY: 只读取该项的 right_ 字段。
    unsafe { ptr::addr_of!((*elem).1.right_).read() }
}

/// 通过裸指针取得节点第 `idx` 项的 key 与可变 value 引用。
///
/// 该函数只对 key 与 value 字段分别做字段级投影，不创建指向整个节点或整个
/// `OrderedArray` 的引用。因此迭代器可以在调用者仍然持有同一节点中其他项
/// 的 `&mut V` 时继续前进。
///
/// # 安全性
///
/// `ptr` 必须指向存活节点，`idx < node_len_raw_(ptr)`；调用方必须保证每个
/// 数据项的 value 最多被交出一个仍在使用中的可变引用。
pub(crate) unsafe fn entry_mut_raw_<'a, const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> (&'a K, &'a mut V) {
    let node = ptr.as_ptr();
    // SAFETY: 只读取 entries_ 中第 idx 项，不创建数组引用。
    let entries = unsafe { ptr::addr_of_mut!((*node).entries_) };
    // SAFETY: 调用方保证 idx 有效。
    let elem = match unsafe { OrderedArray::elem_raw_mut_at_(entries, idx) } {
        Option::Some(elem) => elem,
        Option::None => unreachable!("B 树节点下标应有效"),
    };
    // SAFETY: key 与 value 是同一项内互不重叠的字段。
    let key = unsafe { &*ptr::addr_of!((*elem).0) };
    // SAFETY: 调用方保证同一 value 没有其他仍在使用中的可变引用。
    let value = unsafe { &mut *ptr::addr_of_mut!((*elem).1.value_) };
    (key, value)
}

/// 通过裸指针返回 `child` 在 `parent` 中的孩子下标。
///
/// # 安全性
///
/// `parent` 与 `child` 必须属于同一棵仍然存活的树，且 `child` 是 `parent`
/// 的直接孩子。
pub(crate) unsafe fn child_index_raw_<const M: usize, K, V>(
    parent: TreeNodePtr<M, K, V>,
    child: TreeNodePtr<M, K, V>,
) -> Option<usize> {
    // SAFETY: 调用方保证 parent 存活。
    let len = unsafe { node_len_raw_(parent) };
    (0..=len).find(|&idx| {
        // SAFETY: parent 存活且 idx 在 0..=len 内。
        (unsafe { child_at_raw_(parent, idx) }) == Option::Some(child)
    })
}
