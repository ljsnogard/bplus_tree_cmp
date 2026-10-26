//! B 树删除：后继替换、借位、合并与根收缩。
//!
//! 删除采用递归回溯方式修复下溢：
//!
//! 1. 在节点中定位 key；
//! 2. 若命中的是内部节点，取右子树最小项替换该项，再从右子树删除最小项；
//! 3. 递归返回后检查子节点是否低于最小占用；
//! 4. 优先向左右兄弟借一个项，否则与兄弟合并并删除一个节点；
//! 5. 删除完成后，若 root 为空内部节点则提升其唯一孩子。
//!
//! 递归深度由 B 树的最小 fanout 保证为 `O(log n)`，因此不需要额外的 path
//! SlabList；节点本身仍然全部由 `SlabList` 分配。

use crate::{
    comparer_::TrComparer,
    slab_list_::{SlabList, TrConfig},
};

use super::node_::{
    Node, NodeValue, TreeNodePtr, min_keys_, node_child_at_, node_first_child_, node_is_leaf_,
    node_len_, node_mut_, node_ref_, node_set_first_child_, node_set_parent_,
};

/// 从树中删除与 `query` 等价的项。
///
/// `query_cmp` 用于与查询对象比较，`key_cmp` 用于节点内部排序。
pub(crate) fn remove_entry_from_tree_<const M: usize, K, V, Q, QC, KC, Cfg>(
    nodes: &mut SlabList<Node<M, K, V>, Cfg>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    query: &Q,
    query_cmp: &QC,
    key_cmp: &KC,
    allocator: &Cfg::Allocator,
) -> Option<(K, V)>
where
    Q: ?Sized,
    QC: TrComparer<K, Q>,
    KC: TrComparer<K>,
    Cfg: TrConfig,
{
    let root_ptr = (*root)?;
    let removed = remove_rec_(nodes, root_ptr, query, query_cmp, key_cmp, allocator)?;

    while let Option::Some(root_ptr) = *root {
        let (is_leaf, len) = {
            // SAFETY: root_ptr 由树维护，本块只读取结构信息。
            let node = unsafe { node_ref_(root_ptr) };
            (node.is_leaf_(), node.len_())
        };

        if is_leaf {
            if len == 0 {
                let _ = nodes.remove(root_ptr, allocator);
                *root = Option::None;
            }
            break;
        }

        if len > 0 {
            break;
        }

        let child = node_child_at_(root_ptr, 0);
        let _ = nodes.remove(root_ptr, allocator);
        if let Option::Some(child_ptr) = child {
            node_set_parent_(child_ptr, Option::None);
        }
        *root = child;
    }

    Option::Some(removed)
}

/// 在 `node_ptr` 子树中递归删除与 `query` 等价的项。
fn remove_rec_<const M: usize, K, V, Q, QC, KC, Cfg>(
    nodes: &mut SlabList<Node<M, K, V>, Cfg>,
    node_ptr: TreeNodePtr<M, K, V>,
    query: &Q,
    query_cmp: &QC,
    key_cmp: &KC,
    allocator: &Cfg::Allocator,
) -> Option<(K, V)>
where
    Q: ?Sized,
    QC: TrComparer<K, Q>,
    KC: TrComparer<K>,
    Cfg: TrConfig,
{
    let found = {
        // SAFETY: node_ptr 由树维护，本块只进行只读查找。
        let node = unsafe { node_ref_(node_ptr) };
        node.entries_.binary_search_by(query, query_cmp)
    };

    match found {
        Ok(idx) => {
            let is_leaf = node_is_leaf_(node_ptr);

            if is_leaf {
                let removed = {
                    // SAFETY: node_ptr 由树维护；本块内没有其他引用。
                    let node = unsafe { node_mut_(node_ptr) };
                    node.entries_.try_remove_at(idx)
                };
                return removed.map(|(key, node_value)| (key, node_value.value_));
            }

            // 内部节点：用右子树最小项替换当前项，再从右子树删除最小项。
            let right_child = node_child_at_(node_ptr, idx + 1)?;
            let (succ_key, succ_value) = take_min_(nodes, right_child, key_cmp, allocator)?;

            let removed = {
                // SAFETY: node_ptr 由树维护；本块内没有其他引用。
                let node = unsafe { node_mut_(node_ptr) };
                let old = node.entries_.try_remove_at(idx)?;
                let replacement = (succ_key, NodeValue::new_(succ_value, old.1.right_));
                let result = node.entries_.try_insert_pair_by(replacement, key_cmp);
                debug_assert!(result.is_succ());
                (old.0, old.1.value_)
            };

            // 右子树少了一项，可能低于最小占用。
            rebalance_child_(nodes, node_ptr, idx + 1, key_cmp, allocator);
            Option::Some(removed)
        }
        Err(pos) => {
            // 叶子没有孩子：`child_at_` 返回 None，表示未命中。
            let child = node_child_at_(node_ptr, pos)?;
            let removed = remove_rec_(nodes, child, query, query_cmp, key_cmp, allocator)?;
            rebalance_child_(nodes, node_ptr, pos, key_cmp, allocator);
            Option::Some(removed)
        }
    }
}

/// 从 `node_ptr` 子树中取出最小项，并修复沿途的下溢。
fn take_min_<const M: usize, K, V, KC, Cfg>(
    nodes: &mut SlabList<Node<M, K, V>, Cfg>,
    node_ptr: TreeNodePtr<M, K, V>,
    key_cmp: &KC,
    allocator: &Cfg::Allocator,
) -> Option<(K, V)>
where
    KC: TrComparer<K>,
    Cfg: TrConfig,
{
    let (is_leaf, first_child) = {
        // SAFETY: node_ptr 由树维护，本块只读取结构信息。
        let node = unsafe { node_ref_(node_ptr) };
        (node.is_leaf_(), node.first_child_)
    };

    if is_leaf {
        let removed = {
            // SAFETY: node_ptr 由树维护；本块内没有其他引用。
            let node = unsafe { node_mut_(node_ptr) };
            node.entries_.try_remove_first()
        };
        return removed.map(|(key, node_value)| (key, node_value.value_));
    }

    let child = first_child?;
    let result = take_min_(nodes, child, key_cmp, allocator)?;
    rebalance_child_(nodes, node_ptr, 0, key_cmp, allocator);
    Option::Some(result)
}

/// 检查 `parent_ptr` 的第 `child_idx` 个孩子是否下溢，必要时修复。
fn rebalance_child_<const M: usize, K, V, KC, Cfg>(
    nodes: &mut SlabList<Node<M, K, V>, Cfg>,
    parent_ptr: TreeNodePtr<M, K, V>,
    child_idx: usize,
    key_cmp: &KC,
    allocator: &Cfg::Allocator,
) where
    KC: TrComparer<K>,
    Cfg: TrConfig,
{
    let parent_len = node_len_(parent_ptr);
    let Option::Some(child_ptr) = node_child_at_(parent_ptr, child_idx) else {
        return;
    };

    let min = min_keys_(M);
    if node_len_(child_ptr) >= min {
        return;
    }

    if child_idx > 0 {
        let Option::Some(left_ptr) = node_child_at_(parent_ptr, child_idx - 1) else {
            return;
        };
        if node_len_(left_ptr) > min {
            borrow_left_(parent_ptr, child_idx, left_ptr, child_ptr, key_cmp);
            return;
        }
    }

    if child_idx < parent_len {
        let Option::Some(right_ptr) = node_child_at_(parent_ptr, child_idx + 1) else {
            return;
        };
        if node_len_(right_ptr) > min {
            borrow_right_(parent_ptr, child_idx, right_ptr, child_ptr, key_cmp);
            return;
        }
    }

    if child_idx > 0 {
        merge_with_left_(nodes, parent_ptr, child_idx, key_cmp, allocator);
    } else if child_idx < parent_len {
        merge_with_right_(nodes, parent_ptr, child_idx, key_cmp, allocator);
    }
}

/// 从左兄弟借一个项：父分隔项下移，左兄弟最大项上移。
fn borrow_left_<const M: usize, K, V, KC>(
    parent_ptr: TreeNodePtr<M, K, V>,
    child_idx: usize,
    left_ptr: TreeNodePtr<M, K, V>,
    child_ptr: TreeNodePtr<M, K, V>,
    key_cmp: &KC,
) where
    KC: TrComparer<K>,
{
    let parent_entry = {
        // SAFETY: parent_ptr 由树维护；本块内没有其他引用。
        let parent = unsafe { node_mut_(parent_ptr) };
        parent.entries_.try_remove_at(child_idx - 1)
    };
    let Option::Some(parent_entry) = parent_entry else {
        return;
    };

    let left_entry = {
        // SAFETY: left_ptr 由树维护；本块内没有其他引用。
        let left = unsafe { node_mut_(left_ptr) };
        left.entries_.try_remove_last()
    };
    let Option::Some(left_entry) = left_entry else {
        return;
    };

    let child_first = node_first_child_(child_ptr);
    if let Option::Some(left_last_child) = left_entry.1.right_ {
        node_set_parent_(left_last_child, Option::Some(child_ptr));
    }
    node_set_first_child_(child_ptr, left_entry.1.right_);

    {
        // SAFETY: child_ptr 由树维护；本块内没有其他引用。
        let child = unsafe { node_mut_(child_ptr) };
        let entry = (
            parent_entry.0,
            NodeValue::new_(parent_entry.1.value_, child_first),
        );
        let pos = child.entries_.lower_bound_by(&entry.0, key_cmp);
        let result = child.entries_.try_insert_pair_by(entry, key_cmp);
        debug_assert!(result.is_succ());
        debug_assert_eq!(pos, 0);
    }

    {
        // SAFETY: parent_ptr 由树维护；本块内没有其他引用。
        let parent = unsafe { node_mut_(parent_ptr) };
        let entry = (
            left_entry.0,
            NodeValue::new_(left_entry.1.value_, Option::Some(child_ptr)),
        );
        let pos = parent.entries_.lower_bound_by(&entry.0, key_cmp);
        let result = parent.entries_.try_insert_pair_by(entry, key_cmp);
        debug_assert!(result.is_succ());
        debug_assert_eq!(pos, child_idx - 1);
    }
}

/// 从右兄弟借一个项：父分隔项下移，右兄弟最小项上移。
fn borrow_right_<const M: usize, K, V, KC>(
    parent_ptr: TreeNodePtr<M, K, V>,
    child_idx: usize,
    right_ptr: TreeNodePtr<M, K, V>,
    child_ptr: TreeNodePtr<M, K, V>,
    key_cmp: &KC,
) where
    KC: TrComparer<K>,
{
    let parent_entry = {
        // SAFETY: parent_ptr 由树维护；本块内没有其他引用。
        let parent = unsafe { node_mut_(parent_ptr) };
        parent.entries_.try_remove_at(child_idx)
    };
    let Option::Some(parent_entry) = parent_entry else {
        return;
    };

    let right_entry = {
        // SAFETY: right_ptr 由树维护；本块内没有其他引用。
        let right = unsafe { node_mut_(right_ptr) };
        right.entries_.try_remove_first()
    };
    let Option::Some(right_entry) = right_entry else {
        return;
    };

    let right_first = node_first_child_(right_ptr);
    if let Option::Some(first) = right_first {
        node_set_parent_(first, Option::Some(child_ptr));
    }
    node_set_first_child_(right_ptr, right_entry.1.right_);

    {
        // SAFETY: child_ptr 由树维护；本块内没有其他引用。
        let child = unsafe { node_mut_(child_ptr) };
        let entry = (
            parent_entry.0,
            NodeValue::new_(parent_entry.1.value_, right_first),
        );
        let pos = child.entries_.lower_bound_by(&entry.0, key_cmp);
        let old_len = child.entries_.len();
        let result = child.entries_.try_insert_pair_by(entry, key_cmp);
        debug_assert!(result.is_succ());
        debug_assert_eq!(pos, old_len);
    }

    {
        // SAFETY: parent_ptr 由树维护；本块内没有其他引用。
        let parent = unsafe { node_mut_(parent_ptr) };
        let entry = (
            right_entry.0,
            NodeValue::new_(right_entry.1.value_, Option::Some(right_ptr)),
        );
        let pos = parent.entries_.lower_bound_by(&entry.0, key_cmp);
        let result = parent.entries_.try_insert_pair_by(entry, key_cmp);
        debug_assert!(result.is_succ());
        debug_assert_eq!(pos, child_idx);
    }
}

/// 把当前节点与左兄弟合并，父分隔项放在中间。
fn merge_with_left_<const M: usize, K, V, KC, Cfg>(
    nodes: &mut SlabList<Node<M, K, V>, Cfg>,
    parent_ptr: TreeNodePtr<M, K, V>,
    child_idx: usize,
    key_cmp: &KC,
    allocator: &Cfg::Allocator,
) where
    KC: TrComparer<K>,
    Cfg: TrConfig,
{
    let Option::Some(child_ptr) = node_child_at_(parent_ptr, child_idx) else {
        return;
    };
    let Option::Some(left_ptr) = node_child_at_(parent_ptr, child_idx - 1) else {
        return;
    };

    let parent_entry = {
        // SAFETY: parent_ptr 由树维护；本块内没有其他引用。
        let parent = unsafe { node_mut_(parent_ptr) };
        parent.entries_.try_remove_at(child_idx - 1)
    };
    let Option::Some(parent_entry) = parent_entry else {
        return;
    };

    let child_first = node_first_child_(child_ptr);
    if let Option::Some(first) = child_first {
        node_set_parent_(first, Option::Some(left_ptr));
    }
    {
        // SAFETY: left_ptr 由树维护；本块内没有其他引用。
        let left = unsafe { node_mut_(left_ptr) };
        let entry = (
            parent_entry.0,
            NodeValue::new_(parent_entry.1.value_, child_first),
        );
        let pos = left.entries_.lower_bound_by(&entry.0, key_cmp);
        let old_len = left.entries_.len();
        let result = left.entries_.try_insert_pair_by(entry, key_cmp);
        debug_assert!(result.is_succ());
        debug_assert_eq!(pos, old_len);
    }

    drain_into_(child_ptr, left_ptr, key_cmp);
    let _ = nodes.remove(child_ptr, allocator);
}

/// 把右兄弟并入当前节点，父分隔项放在中间。
fn merge_with_right_<const M: usize, K, V, KC, Cfg>(
    nodes: &mut SlabList<Node<M, K, V>, Cfg>,
    parent_ptr: TreeNodePtr<M, K, V>,
    child_idx: usize,
    key_cmp: &KC,
    allocator: &Cfg::Allocator,
) where
    KC: TrComparer<K>,
    Cfg: TrConfig,
{
    let Option::Some(child_ptr) = node_child_at_(parent_ptr, child_idx) else {
        return;
    };
    let Option::Some(right_ptr) = node_child_at_(parent_ptr, child_idx + 1) else {
        return;
    };

    let parent_entry = {
        // SAFETY: parent_ptr 由树维护；本块内没有其他引用。
        let parent = unsafe { node_mut_(parent_ptr) };
        parent.entries_.try_remove_at(child_idx)
    };
    let Option::Some(parent_entry) = parent_entry else {
        return;
    };

    let right_first = node_first_child_(right_ptr);
    if let Option::Some(first) = right_first {
        node_set_parent_(first, Option::Some(child_ptr));
    }
    {
        // SAFETY: child_ptr 由树维护；本块内没有其他引用。
        let child = unsafe { node_mut_(child_ptr) };
        let entry = (
            parent_entry.0,
            NodeValue::new_(parent_entry.1.value_, right_first),
        );
        let pos = child.entries_.lower_bound_by(&entry.0, key_cmp);
        let old_len = child.entries_.len();
        let result = child.entries_.try_insert_pair_by(entry, key_cmp);
        debug_assert!(result.is_succ());
        debug_assert_eq!(pos, old_len);
    }

    drain_into_(right_ptr, child_ptr, key_cmp);
    let _ = nodes.remove(right_ptr, allocator);
}

/// 把 `from` 节点的全部项按顺序搬到 `to` 节点末尾。
fn drain_into_<const M: usize, K, V, KC>(
    from: TreeNodePtr<M, K, V>,
    to: TreeNodePtr<M, K, V>,
    key_cmp: &KC,
) where
    KC: TrComparer<K>,
{
    loop {
        let entry = {
            // SAFETY: from 由树维护；本块内没有其他引用。
            let node = unsafe { node_mut_(from) };
            node.entries_.try_remove_first()
        };
        let Option::Some(entry) = entry else {
            break;
        };

        if let Option::Some(child) = entry.1.right_ {
            node_set_parent_(child, Option::Some(to));
        }

        let result = {
            // SAFETY: to 由树维护；本块内没有其他引用。
            let node = unsafe { node_mut_(to) };
            node.entries_.try_insert_pair_by(entry, key_cmp)
        };
        debug_assert!(result.is_succ());
    }
}
