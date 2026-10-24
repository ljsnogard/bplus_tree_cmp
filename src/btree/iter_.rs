//! B 树中序迭代器。
//!
//! B Tree 的数据分散在所有层级的节点中。本实现利用节点中的父指针计算中序
//! 后继，不需要工作栈：
//!
//! - 当前项在内部节点中时，后继是它右子树的最小项；
//! - 当前项在叶子中时，后继是同叶子的下一项，或者沿父链找到第一个
//!   “当前孩子不是最后一个孩子”的祖先，取该祖先中对应的项；
//! - 迭代器只保存当前 `(节点, 下标)`，不进行任何堆分配。
//!
//! 因此 B 树节点和迭代器状态都只通过树结构本身与 `SlabList` 管理，没有
//! `Vec` 或其他额外堆分配。

use core::{iter::FusedIterator, marker::PhantomData};

use super::node_::{Node, TreeNodePtr, child_index_, node_parent_, node_ref_};

/// 按 key 升序迭代 [`super::BTree`]。
///
/// 迭代器借用树，因此不能在迭代期间修改树。父指针由树结构维护，迭代器
/// 本身不需要额外工作栈。
pub struct TreeIter<'a, const M: usize, K, V = ()> {
    /// 当前项的节点与节点内下标；`None` 表示迭代结束。
    current_: Option<(TreeNodePtr<M, K, V>, usize)>,
    /// 仍未访问的项数。
    remaining_: usize,
    _mark_: PhantomData<&'a (K, V)>,
}

impl<'a, const M: usize, K, V> TreeIter<'a, M, K, V> {
    /// 创建一个中序迭代器。
    pub(crate) fn new_(root: Option<TreeNodePtr<M, K, V>>, len: usize) -> Self {
        TreeIter {
            current_: root.map(leftmost_),
            remaining_: len,
            _mark_: PhantomData,
        }
    }
}

impl<'a, const M: usize, K, V> Iterator for TreeIter<'a, M, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        let (node_ptr, idx) = self.current_?;

        // SAFETY: node_ptr 由树维护；迭代器借用树，节点在 'a 内保持存活。
        let node: &'a Node<M, K, V> = unsafe { node_ref_(node_ptr) };
        let (key, node_value) = node.entries_.get_ref_at(idx).ok()?;

        self.current_ = next_entry_(node_ptr, idx);
        self.remaining_ -= 1;
        Option::Some((key, &node_value.value_))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining_, Option::Some(self.remaining_))
    }
}

impl<const M: usize, K, V> ExactSizeIterator for TreeIter<'_, M, K, V> {}

impl<const M: usize, K, V> FusedIterator for TreeIter<'_, M, K, V> {}

/// 返回以 `node_ptr` 为根的最左项的 `(节点, 下标)`。
fn leftmost_<const M: usize, K, V>(
    mut node_ptr: TreeNodePtr<M, K, V>,
) -> (TreeNodePtr<M, K, V>, usize) {
    loop {
        // SAFETY: node_ptr 由树维护。
        let node = unsafe { node_ref_(node_ptr) };

        if node.is_leaf_() {
            return (node_ptr, 0);
        }

        let Option::Some(child) = node.first_child_ else {
            return (node_ptr, 0);
        };
        node_ptr = child;
    }
}

/// 返回 `(node_ptr, idx)` 的中序后继。
fn next_entry_<const M: usize, K, V>(
    node_ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> Option<(TreeNodePtr<M, K, V>, usize)> {
    // SAFETY: node_ptr 由树维护。
    let node = unsafe { node_ref_(node_ptr) };

    // 内部节点的当前项之后一定是它右子树的最小项。
    if !node.is_leaf_()
        && let Option::Some(right_child) = node.child_at_(idx + 1)
    {
        return Option::Some(leftmost_(right_child));
    }

    // 叶子中还有下一项。
    if idx + 1 < node.len_() {
        return Option::Some((node_ptr, idx + 1));
    }

    // 已经是叶子最后一项：沿父链寻找第一个“当前孩子不是最后一个孩子”的祖先。
    let mut child = node_ptr;
    loop {
        let parent = node_parent_(child)?;
        let child_idx = child_index_(parent, child)?;

        // SAFETY: parent 由树维护。
        let parent_node = unsafe { node_ref_(parent) };
        if child_idx < parent_node.len_() {
            return Option::Some((parent, child_idx));
        }

        child = parent;
    }
}
