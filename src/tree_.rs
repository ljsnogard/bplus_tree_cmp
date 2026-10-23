//! 基于 Comparer 定序的 B+ Tree。
//!
//! 树中的节点只保存 `SlotPtr`，不复制 `K`。排序完全由 [`TrComparer`]
//! 定义；查询对象可以与 `K` 不同。
//!
//! # 结构
//!
//! - 数据存放在 `data_arena_` 的 `SlabList<(K, V)>` 中；
//! - 节点存放在 `node_arena_` 的 `SlabList<TreeNode>` 中；
//! - 索引项为 `(子树最大 key 的 SlotPtr, 孩子指针)`；
//! - 叶子通过 `prev` / `next` 组成有序链，树自己持有首尾叶指针。
//!
//! # Comparer 一致性
//!
//! 树中元素的顺序由构造时传入的 `C` 固定。`get_by` / `remove_entry_by` /
//! `try_insert_by` 等接受额外 Comparer 的方法，其 Comparer 必须在 `K ↔ K`
//! 上与 `C` 一致，否则查找和插入位置没有意义。
//!
//! # 示例
//!
//! ```
//! use bplustree_cmp::{BPlusTree, OrdComparer};
//!
//! let mut tree = BPlusTree::<4, _, u64, &str>::new_in(OrdComparer::new()).unwrap();
//! tree.insert(2, "two");
//! tree.insert(1, "one");
//! tree.insert(3, "three");
//!
//! let pairs: std::vec::Vec<_> = tree.iter().map(|(k, v)| (*k, *v)).collect();
//! assert_eq!(pairs, std::vec![(1, "one"), (2, "two"), (3, "three")]);
//! ```

use alloc::alloc::Global;
use core::{borrow::Borrow, iter::FusedIterator, marker::PhantomData, mem};

use crate::{
    comparer_::{QuerySlotComparer, SlotPairKeyComparer, TrComparer},
    node_::{DataLeaf, IndexNode, IndexPtrOrdArr, TreeNode, TreeNodePtr},
    ordered_arr_::OrderedArray,
    search_::{DescentPath, descend_to_leaf_, find_in_leaf_},
    slab_list_::{SlabList, SlabListError, SlotPtr},
};

/// 非根节点的最小项数。
#[inline]
const fn min_occupancy_(degree: usize) -> usize {
    degree / 2
}

/// 分裂时左侧保留的原有项数。
#[inline]
const fn split_left_len_(degree: usize) -> usize {
    degree.div_ceil(2)
}

/// 通过 `SlotPtr` 取得节点引用。
///
/// # 安全性
///
/// - `ptr` 必须指向仍然存活、且属于当前树的节点；
/// - 在返回引用的生命周期内，不能存在指向同一节点的可变引用。
unsafe fn node_ref_<'a, const M: usize, K, V>(ptr: TreeNodePtr<M, K, V>) -> &'a TreeNode<M, K, V> {
    // SAFETY: 调用方保证 ptr 指向存活节点。
    unsafe { &*ptr.as_ptr() }
}

/// 通过 `SlotPtr` 取得节点可变引用。
///
/// # 安全性
///
/// - `ptr` 必须指向仍然存活、且属于当前树的节点；
/// - 在返回引用的生命周期内，不能存在指向同一节点的其他引用。
unsafe fn node_mut_<'a, const M: usize, K, V>(
    ptr: TreeNodePtr<M, K, V>,
) -> &'a mut TreeNode<M, K, V> {
    // SAFETY: 调用方保证 ptr 指向存活节点且没有别名。
    unsafe { &mut *ptr.as_ptr() }
}

/// 通过 `SlotPtr` 取得数据引用。
///
/// # 安全性
///
/// `ptr` 必须指向仍然存活的数据槽位；在返回引用的生命周期内不能释放该槽位。
unsafe fn data_pair_<'a, K, V>(ptr: SlotPtr<(K, V)>) -> (&'a K, &'a V) {
    // SAFETY: 调用方保证数据槽位仍然存活。
    let (key, value) = unsafe { &*ptr.as_ptr() };
    (key, value)
}

/// 通过 `SlotPtr` 取得数据可变引用。
///
/// # 安全性
///
/// `ptr` 必须指向仍然存活的数据槽位，且调用方必须拥有对该槽位的独占访问权。
unsafe fn data_pair_mut_<'a, K, V>(ptr: SlotPtr<(K, V)>) -> (&'a K, &'a mut V) {
    // SAFETY: 调用方保证数据槽位存活且没有别名。
    let (key, value) = unsafe { &mut *ptr.as_ptr() };
    (&*key, value)
}

/// 返回节点项数。
fn node_len_<const M: usize, K, V>(ptr: TreeNodePtr<M, K, V>) -> usize {
    // SAFETY: 调用方传入的节点指针由树维护。
    unsafe { node_ref_(ptr) }.len_()
}

/// 返回节点是否为叶子。
fn node_is_leaf_<const M: usize, K, V>(ptr: TreeNodePtr<M, K, V>) -> bool {
    // SAFETY: 调用方传入的节点指针由树维护。
    unsafe { node_ref_(ptr) }.is_leaf_()
}

/// 返回节点最大数据 key 的句柄。
fn node_max_<const M: usize, K, V>(ptr: TreeNodePtr<M, K, V>) -> Option<SlotPtr<(K, V)>> {
    // SAFETY: 调用方传入的节点指针由树维护。
    unsafe { node_ref_(ptr) }.max_key_()
}

/// 返回索引节点第 `idx` 个孩子。
fn child_at_<const M: usize, K, V>(
    parent: TreeNodePtr<M, K, V>,
    idx: usize,
) -> Option<TreeNodePtr<M, K, V>> {
    // SAFETY: 调用方传入的节点指针由树维护。
    let node = unsafe { node_ref_(parent) };
    let index = node.as_index_()?;
    let (_, child) = index.children_.get_ref_at(idx).ok()?;
    Option::Some(*child)
}

/// 把索引节点第 `idx` 项的分隔 key 替换为 `new_sep`，保留原来的孩子指针。
///
/// `OrderedArray` 不支持原地替换 key，因此先移除再按顺序插入。由于新 key
/// 仍然落在原位置的区间内，插入后应回到同一个下标。
fn set_separator_at_<const M: usize, K, V, C>(
    children: &mut IndexPtrOrdArr<M, K, V>,
    idx: usize,
    new_sep: SlotPtr<(K, V)>,
    cmp: &C,
) where
    C: TrComparer<SlotPtr<(K, V)>>,
{
    let old_sep = match children.get_ref_at(idx) {
        Ok((key, _)) => *key,
        Err(_) => return,
    };
    if old_sep == new_sep {
        return;
    }

    let Some((_, child)) = children.try_remove_at(idx) else {
        return;
    };
    let result = children.try_insert_pair_by((new_sep, child), cmp);
    debug_assert!(result.is_succ());
    debug_assert_eq!(
        children
            .get_ref_at(idx)
            .map(|(key, child)| (*key, *child))
            .ok(),
        Option::Some((new_sep, child))
    );
}

/// 从 `depth` 位置开始向上刷新 separator。
///
/// `depth` 是当前节点在 path 中的深度；它的父节点是 `path[depth - 1]`。
/// 当父节点的最大 key 没有变化时提前停止，因为更上层的 separator 不会变化。
fn refresh_separators_<const M: usize, K, V, C>(
    path: &[(TreeNodePtr<M, K, V>, usize)],
    mut depth: usize,
    mut child_max: SlotPtr<(K, V)>,
    cmp: &C,
) where
    C: TrComparer<SlotPtr<(K, V)>>,
{
    while depth > 0 {
        let (parent_ptr, idx) = path[depth - 1];
        let old_parent_max = node_max_(parent_ptr);

        {
            // SAFETY: parent_ptr 由 path 来自当前树；此处没有其他别名。
            let parent = match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                Option::Some(parent) => parent,
                Option::None => return,
            };
            set_separator_at_(&mut parent.children_, idx, child_max, cmp);
        }

        let new_parent_max = node_max_(parent_ptr);
        if new_parent_max == old_parent_max {
            break;
        }
        let Option::Some(new_max) = new_parent_max else {
            break;
        };
        child_max = new_max;
        depth -= 1;
    }
}

/// 把 `arr` 的右侧项搬到新数组，并把 `pending` 插入到合适的一侧。
///
/// 调用前 `arr` 必须已满。该函数不引入 `M + 1` 的临时容量。
fn split_array_with_<const M: usize, K, V, C>(
    arr: &mut OrderedArray<M, K, V>,
    pending: (K, V),
    cmp: &C,
) -> OrderedArray<M, K, V>
where
    C: TrComparer<K>,
{
    debug_assert_eq!(arr.len(), M);

    let split_at = split_left_len_(M);
    let insert_pos = arr.lower_bound_by(&pending.0, cmp);

    let mut right = OrderedArray::new();
    while arr.len() > split_at {
        let Option::Some((key, value)) = arr.try_remove_last() else {
            break;
        };
        let result = right.try_insert_pair_by((key, value), cmp);
        debug_assert!(result.is_succ());
    }

    if insert_pos < split_at {
        let result = arr.try_insert_pair_by(pending, cmp);
        debug_assert!(result.is_succ());
    } else {
        let result = right.try_insert_pair_by(pending, cmp);
        debug_assert!(result.is_succ());
    }

    right
}

/// 叶子分裂结果：左子树最大 key、右子树最大 key、新右叶、旧后继。
type SplitLeafResult<const M: usize, K, V> = (
    SlotPtr<(K, V)>,
    SlotPtr<(K, V)>,
    TreeNodePtr<M, K, V>,
    Option<TreeNodePtr<M, K, V>>,
);

/// 索引分裂结果：左子树最大 key、右子树最大 key、新右索引。
type SplitIndexResult<const M: usize, K, V> =
    (SlotPtr<(K, V)>, SlotPtr<(K, V)>, TreeNodePtr<M, K, V>);

/// 分裂一个已满的叶子，返回左右子树最大 key 的新右叶子指针，以及旧后继。
fn split_leaf_with_<const M: usize, K, V, C>(
    nodes: &mut SlabList<TreeNode<M, K, V>>,
    leaf_ptr: TreeNodePtr<M, K, V>,
    pending: SlotPtr<(K, V)>,
    cmp: &C,
) -> SplitLeafResult<M, K, V>
where
    C: TrComparer<SlotPtr<(K, V)>>,
{
    let old_next = match unsafe { node_ref_(leaf_ptr) }.as_leaf_() {
        Option::Some(leaf) => leaf.next_sibl_,
        Option::None => unreachable!("split_leaf_with_ 只接受叶子节点"),
    };

    let (left_max, right_max, right_arr) = {
        let leaf = match unsafe { node_mut_(leaf_ptr) }.as_leaf_mut_() {
            Option::Some(leaf) => leaf,
            Option::None => unreachable!("split_leaf_with_ 只接受叶子节点"),
        };
        let right_arr = split_array_with_(&mut leaf.order_arr_, (pending, ()), cmp);
        let left_max = match leaf.order_arr_.last() {
            Option::Some((key, _)) => *key,
            Option::None => unreachable!("分裂后的左叶子不应为空"),
        };
        let right_max = match right_arr.last() {
            Option::Some((key, _)) => *key,
            Option::None => unreachable!("分裂后的右叶子不应为空"),
        };
        (left_max, right_max, right_arr)
    };

    let right_ptr = nodes.insert(TreeNode::Leaf(DataLeaf {
        order_arr_: right_arr,
        prev_sibl_: Option::Some(leaf_ptr),
        next_sibl_: old_next,
    }));

    match unsafe { node_mut_(leaf_ptr) }.as_leaf_mut_() {
        Option::Some(leaf) => leaf.next_sibl_ = Option::Some(right_ptr),
        Option::None => unreachable!("split_leaf_with_ 只接受叶子节点"),
    }
    if let Option::Some(next_ptr) = old_next {
        match unsafe { node_mut_(next_ptr) }.as_leaf_mut_() {
            Option::Some(next) => next.prev_sibl_ = Option::Some(right_ptr),
            Option::None => unreachable!("叶子链不应指向非叶子节点"),
        }
    }

    (left_max, right_max, right_ptr, old_next)
}

/// 分裂一个已满的索引节点，返回左右子树最大 key 与新右索引指针。
fn split_index_with_<const M: usize, K, V, C>(
    nodes: &mut SlabList<TreeNode<M, K, V>>,
    index_ptr: TreeNodePtr<M, K, V>,
    pending: (SlotPtr<(K, V)>, TreeNodePtr<M, K, V>),
    cmp: &C,
) -> SplitIndexResult<M, K, V>
where
    C: TrComparer<SlotPtr<(K, V)>>,
{
    let (left_max, right_max, right_arr) = {
        let index = match unsafe { node_mut_(index_ptr) }.as_index_mut_() {
            Option::Some(index) => index,
            Option::None => unreachable!("split_index_with_ 只接受索引节点"),
        };
        let right_arr = split_array_with_(&mut index.children_, pending, cmp);
        let left_max = match index.children_.last() {
            Option::Some((key, _)) => *key,
            Option::None => unreachable!("分裂后的左索引不应为空"),
        };
        let right_max = match right_arr.last() {
            Option::Some((key, _)) => *key,
            Option::None => unreachable!("分裂后的右索引不应为空"),
        };
        (left_max, right_max, right_arr)
    };

    let right_ptr = nodes.insert(TreeNode::Index(IndexNode {
        children_: right_arr,
    }));

    (left_max, right_max, right_ptr)
}

/// 用两个 `(separator, child)` 项创建新的索引 root。
fn create_root_with_two_<const M: usize, K, V, C>(
    nodes: &mut SlabList<TreeNode<M, K, V>>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    left_ptr: TreeNodePtr<M, K, V>,
    left_max: SlotPtr<(K, V)>,
    right_ptr: TreeNodePtr<M, K, V>,
    right_max: SlotPtr<(K, V)>,
    cmp: &C,
) where
    C: TrComparer<SlotPtr<(K, V)>>,
{
    let mut index = IndexNode::new_();
    let result = index
        .children_
        .try_insert_pair_by((left_max, left_ptr), cmp);
    debug_assert!(result.is_succ());
    let result = index
        .children_
        .try_insert_pair_by((right_max, right_ptr), cmp);
    debug_assert!(result.is_succ());

    *root = Option::Some(nodes.insert(TreeNode::Index(index)));
}

/// 把新数据句柄插入树中；数据本身已经写入 data arena。
///
/// 该函数负责叶子插入、分裂、向上传播以及根分裂。
fn insert_data_into_tree_<const M: usize, K, V, C>(
    nodes: &mut SlabList<TreeNode<M, K, V>>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    last_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    leaf_ptr: TreeNodePtr<M, K, V>,
    path: DescentPath<M, K, V>,
    data_ptr: SlotPtr<(K, V)>,
    cmp: &C,
) where
    C: TrComparer<SlotPtr<(K, V)>>,
{
    let leaf_len = node_len_(leaf_ptr);
    debug_assert!(leaf_len <= M);

    if leaf_len < M {
        match unsafe { node_mut_(leaf_ptr) }.as_leaf_mut_() {
            Option::Some(leaf) => {
                let result = leaf.order_arr_.try_insert_pair_by((data_ptr, ()), cmp);
                debug_assert!(result.is_succ());
            }
            Option::None => unreachable!("叶子指针应指向叶子节点"),
        }

        let Option::Some(leaf_max) = node_max_(leaf_ptr) else {
            return;
        };
        refresh_separators_(&path, path.len(), leaf_max, cmp);
        return;
    }

    let (left_max, right_max, right_ptr, old_next) =
        split_leaf_with_(nodes, leaf_ptr, data_ptr, cmp);
    if old_next.is_none() {
        *last_leaf = Option::Some(right_ptr);
    }

    let mut pending = Option::Some((right_max, right_ptr));
    let mut child_max = left_max;

    if path.is_empty() {
        create_root_with_two_(nodes, root, leaf_ptr, left_max, right_ptr, right_max, cmp);
        return;
    }

    let mut level = path.len();
    while level > 0 {
        let (parent_ptr, idx) = path[level - 1];
        let parent_len = node_len_(parent_ptr);
        let old_parent_max = node_max_(parent_ptr);

        match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
            Option::Some(parent) => {
                set_separator_at_(&mut parent.children_, idx, child_max, cmp);
            }
            Option::None => unreachable!("路径中的父节点应为索引节点"),
        }

        match pending.take() {
            Option::Some((right_sep, right_child)) => {
                if parent_len < M {
                    match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                        Option::Some(parent) => {
                            let result = parent
                                .children_
                                .try_insert_pair_by((right_sep, right_child), cmp);
                            debug_assert!(result.is_succ());
                        }
                        Option::None => unreachable!("路径中的父节点应为索引节点"),
                    }

                    let new_parent_max = node_max_(parent_ptr);
                    if new_parent_max == old_parent_max {
                        break;
                    }
                    let Option::Some(new_max) = new_parent_max else {
                        break;
                    };
                    child_max = new_max;
                    level -= 1;
                } else {
                    let (left, right, right_node) =
                        split_index_with_(nodes, parent_ptr, (right_sep, right_child), cmp);
                    pending = Option::Some((right, right_node));
                    child_max = left;
                    level -= 1;
                }
            }
            Option::None => {
                let new_parent_max = node_max_(parent_ptr);
                if new_parent_max == old_parent_max {
                    break;
                }
                let Option::Some(new_max) = new_parent_max else {
                    break;
                };
                child_max = new_max;
                level -= 1;
            }
        }
    }

    if let Option::Some((right_sep, right_child)) = pending {
        let Option::Some(old_root) = *root else {
            return;
        };
        let Option::Some(left_max) = node_max_(old_root) else {
            return;
        };
        create_root_with_two_(nodes, root, old_root, left_max, right_child, right_sep, cmp);
    }
}

/// 把一个叶子的全部项搬到另一个叶子。
fn move_leaf_entries_<const M: usize, K, V, C>(
    from_ptr: TreeNodePtr<M, K, V>,
    to_ptr: TreeNodePtr<M, K, V>,
    cmp: &C,
) where
    C: TrComparer<SlotPtr<(K, V)>>,
{
    loop {
        let moved = match unsafe { node_mut_(from_ptr) }.as_leaf_mut_() {
            Option::Some(from) => from.order_arr_.try_remove_first(),
            Option::None => unreachable!("move_leaf_entries_ 只接受叶子节点"),
        };
        let Option::Some((key, ())) = moved else {
            break;
        };

        match unsafe { node_mut_(to_ptr) }.as_leaf_mut_() {
            Option::Some(to) => {
                let result = to.order_arr_.try_insert_pair_by((key, ()), cmp);
                debug_assert!(result.is_succ());
            }
            Option::None => unreachable!("move_leaf_entries_ 只接受叶子节点"),
        }
    }
}

/// 把一个索引节点的全部项搬到另一个索引节点。
fn move_index_entries_<const M: usize, K, V, C>(
    from_ptr: TreeNodePtr<M, K, V>,
    to_ptr: TreeNodePtr<M, K, V>,
    cmp: &C,
) where
    C: TrComparer<SlotPtr<(K, V)>>,
{
    loop {
        let moved = match unsafe { node_mut_(from_ptr) }.as_index_mut_() {
            Option::Some(from) => from.children_.try_remove_first(),
            Option::None => unreachable!("move_index_entries_ 只接受索引节点"),
        };
        let Option::Some(pair) = moved else {
            break;
        };

        match unsafe { node_mut_(to_ptr) }.as_index_mut_() {
            Option::Some(to) => {
                let result = to.children_.try_insert_pair_by(pair, cmp);
                debug_assert!(result.is_succ());
            }
            Option::None => unreachable!("move_index_entries_ 只接受索引节点"),
        }
    }
}

/// 从叶子链中摘除一个叶子，并维护树持有的首尾叶指针。
fn unlink_leaf_<const M: usize, K, V>(
    leaf_ptr: TreeNodePtr<M, K, V>,
    first_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    last_leaf: &mut Option<TreeNodePtr<M, K, V>>,
) {
    let (prev, next) = match unsafe { node_ref_(leaf_ptr) }.as_leaf_() {
        Option::Some(leaf) => (leaf.prev_sibl_, leaf.next_sibl_),
        Option::None => return,
    };

    if let Option::Some(prev_ptr) = prev {
        if let Option::Some(prev) = unsafe { node_mut_(prev_ptr) }.as_leaf_mut_() {
            prev.next_sibl_ = next;
        }
    } else {
        *first_leaf = next;
    }

    if let Option::Some(next_ptr) = next {
        if let Option::Some(next) = unsafe { node_mut_(next_ptr) }.as_leaf_mut_() {
            next.prev_sibl_ = prev;
        }
    } else {
        *last_leaf = prev;
    }
}

/// 把 root 收缩为它唯一的孩子。
fn collapse_root_to_child_<const M: usize, K, V>(
    nodes: &mut SlabList<TreeNode<M, K, V>>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    first_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    last_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    root_ptr: TreeNodePtr<M, K, V>,
    child_ptr: TreeNodePtr<M, K, V>,
) {
    let mut old_root = root_ptr;
    let mut child = child_ptr;

    loop {
        *root = Option::Some(child);
        let _ = nodes.remove(old_root);

        if node_is_leaf_(child) {
            *first_leaf = Option::Some(child);
            *last_leaf = Option::Some(child);
            return;
        }

        // 对于偶数 M，非根索引至少有两个孩子；奇数 M 允许一元链。
        // root 必须满足“要么是叶子，要么至少两个孩子”，因此继续收缩一元链。
        if node_len_(child) != 1 {
            return;
        }

        let Option::Some(grandchild) = child_at_(child, 0) else {
            return;
        };
        old_root = child;
        child = grandchild;
    }
}

/// 清空树的 root 与首尾叶指针，并释放 root 节点。
fn clear_root_<const M: usize, K, V>(
    nodes: &mut SlabList<TreeNode<M, K, V>>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    first_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    last_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    root_ptr: TreeNodePtr<M, K, V>,
) {
    let _ = nodes.remove(root_ptr);
    *root = Option::None;
    *first_leaf = Option::None;
    *last_leaf = Option::None;
}

/// 在删除一项后修复节点占用与 separator。
///
/// `depth` 是 `current_ptr` 在 `path` 中的深度；叶子的深度是 `path.len()`。
/// 该函数只处理树结构，不负责从 data arena 删除元素。
#[allow(clippy::too_many_arguments)]
#[allow(clippy::collapsible_if)] // 保留“先左后右”的借位语义，避免 let-chain 降低可读性
fn rebalance_after_remove_<const M: usize, K, V, C>(
    nodes: &mut SlabList<TreeNode<M, K, V>>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    first_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    last_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    path: &[(TreeNodePtr<M, K, V>, usize)],
    mut current_ptr: TreeNodePtr<M, K, V>,
    mut current_max: Option<SlotPtr<(K, V)>>,
    mut depth: usize,
    cmp: &C,
) where
    C: TrComparer<SlotPtr<(K, V)>>,
{
    let min = min_occupancy_(M);

    loop {
        // 1. 当前节点已经是 root。
        if depth == 0 {
            let (is_leaf, node_len) = {
                let node = unsafe { node_ref_(current_ptr) };
                (node.is_leaf_(), node.len_())
            };

            if is_leaf {
                if node_len == 0 {
                    unlink_leaf_(current_ptr, first_leaf, last_leaf);
                    clear_root_(nodes, root, first_leaf, last_leaf, current_ptr);
                }
                return;
            }

            match node_len {
                0 => clear_root_(nodes, root, first_leaf, last_leaf, current_ptr),
                1 => {
                    let child_ptr = match child_at_(current_ptr, 0) {
                        Option::Some(child) => child,
                        Option::None => return,
                    };
                    collapse_root_to_child_(
                        nodes,
                        root,
                        first_leaf,
                        last_leaf,
                        current_ptr,
                        child_ptr,
                    );
                }
                _ => {}
            }
            return;
        }

        let (parent_ptr, idx) = path[depth - 1];

        // 2. root 只有一个孩子：root 收缩为该孩子。
        if depth == 1 && node_len_(parent_ptr) == 1 {
            if node_len_(current_ptr) == 0 {
                if node_is_leaf_(current_ptr) {
                    unlink_leaf_(current_ptr, first_leaf, last_leaf);
                }
                let _ = nodes.remove(current_ptr);
                clear_root_(nodes, root, first_leaf, last_leaf, parent_ptr);
            } else {
                collapse_root_to_child_(
                    nodes,
                    root,
                    first_leaf,
                    last_leaf,
                    parent_ptr,
                    current_ptr,
                );
            }
            return;
        }

        // 3. 当前节点不再低于最小占用：刷新 separator 后结束。
        if node_len_(current_ptr) >= min {
            if let Option::Some(max) = current_max {
                refresh_separators_(path, depth, max, cmp);
            }
            return;
        }

        // 4. 父节点只有一个孩子：折叠这一层冗余的索引。
        if node_len_(parent_ptr) == 1 {
            if node_len_(current_ptr) == 0 {
                if node_is_leaf_(current_ptr) {
                    unlink_leaf_(current_ptr, first_leaf, last_leaf);
                }
                let _ = nodes.remove(current_ptr);

                match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                    Option::Some(parent) => {
                        let _ = parent.children_.try_remove_at(0);
                    }
                    Option::None => return,
                }

                current_ptr = parent_ptr;
                current_max = Option::None;
                depth -= 1;
                continue;
            }

            // 父节点只有当前孩子，直接用当前节点替换父节点。
            if depth < 2 {
                return;
            }
            let (grand_ptr, grand_idx) = path[depth - 2];
            match unsafe { node_mut_(grand_ptr) }.as_index_mut_() {
                Option::Some(grand) => match grand.children_.get_mut_at(grand_idx) {
                    Ok((_, child_slot)) => *child_slot = current_ptr,
                    Err(_) => return,
                },
                Option::None => return,
            }
            let _ = nodes.remove(parent_ptr);
            current_max = node_max_(current_ptr);
            depth -= 1;
            continue;
        }

        let parent_len = node_len_(parent_ptr);
        let left_ptr = if idx > 0 {
            child_at_(parent_ptr, idx - 1)
        } else {
            Option::None
        };
        let right_ptr = if idx + 1 < parent_len {
            child_at_(parent_ptr, idx + 1)
        } else {
            Option::None
        };

        let current_len = node_len_(current_ptr);

        if node_is_leaf_(current_ptr) {
            // 5. 尝试向兄弟借一个，空节点直接走合并。
            if current_len > 0 {
                if let Option::Some(left) = left_ptr {
                    if node_len_(left) > min {
                        let moved = match unsafe { node_mut_(left) }.as_leaf_mut_() {
                            Option::Some(left) => left.order_arr_.try_remove_last(),
                            Option::None => unreachable!("左兄弟应为叶子"),
                        };
                        if let Option::Some((key, ())) = moved {
                            match unsafe { node_mut_(current_ptr) }.as_leaf_mut_() {
                                Option::Some(current) => {
                                    let result =
                                        current.order_arr_.try_insert_pair_by((key, ()), cmp);
                                    debug_assert!(result.is_succ());
                                }
                                Option::None => unreachable!("当前节点应为叶子"),
                            }
                            let left_max = node_max_(left);
                            let current_max = node_max_(current_ptr);
                            if let (Option::Some(left_max), Option::Some(current_max)) =
                                (left_max, current_max)
                            {
                                match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                                    Option::Some(parent) => {
                                        set_separator_at_(
                                            &mut parent.children_,
                                            idx - 1,
                                            left_max,
                                            cmp,
                                        );
                                        set_separator_at_(
                                            &mut parent.children_,
                                            idx,
                                            current_max,
                                            cmp,
                                        );
                                    }
                                    Option::None => return,
                                }
                                let parent_max = node_max_(parent_ptr);
                                if let Option::Some(parent_max) = parent_max {
                                    refresh_separators_(path, depth - 1, parent_max, cmp);
                                }
                            }
                            return;
                        }
                    }
                } else if let Option::Some(right) = right_ptr {
                    if node_len_(right) > min {
                        let moved = match unsafe { node_mut_(right) }.as_leaf_mut_() {
                            Option::Some(right) => right.order_arr_.try_remove_first(),
                            Option::None => unreachable!("右兄弟应为叶子"),
                        };
                        if let Option::Some((key, ())) = moved {
                            match unsafe { node_mut_(current_ptr) }.as_leaf_mut_() {
                                Option::Some(current) => {
                                    let result =
                                        current.order_arr_.try_insert_pair_by((key, ()), cmp);
                                    debug_assert!(result.is_succ());
                                }
                                Option::None => unreachable!("当前节点应为叶子"),
                            }
                            let current_max = node_max_(current_ptr);
                            let right_max = node_max_(right);
                            if let (Option::Some(current_max), Option::Some(right_max)) =
                                (current_max, right_max)
                            {
                                match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                                    Option::Some(parent) => {
                                        set_separator_at_(
                                            &mut parent.children_,
                                            idx,
                                            current_max,
                                            cmp,
                                        );
                                        set_separator_at_(
                                            &mut parent.children_,
                                            idx + 1,
                                            right_max,
                                            cmp,
                                        );
                                    }
                                    Option::None => return,
                                }
                                let parent_max = node_max_(parent_ptr);
                                if let Option::Some(parent_max) = parent_max {
                                    refresh_separators_(path, depth - 1, parent_max, cmp);
                                }
                            }
                            return;
                        }
                    }
                }
            }

            // 6. 合并叶子。
            let (survivor, victim, victim_idx) = match (left_ptr, right_ptr) {
                (Option::Some(left), _) => (left, current_ptr, idx),
                (Option::None, Option::Some(right)) => (current_ptr, right, idx + 1),
                (Option::None, Option::None) => return,
            };

            move_leaf_entries_(victim, survivor, cmp);
            unlink_leaf_(victim, first_leaf, last_leaf);
            let _ = nodes.remove(victim);

            match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                Option::Some(parent) => {
                    let _ = parent.children_.try_remove_at(victim_idx);
                }
                Option::None => return,
            }
            let survivor_idx = if victim_idx == idx { idx - 1 } else { idx };
            if let Option::Some(survivor_max) = node_max_(survivor) {
                match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                    Option::Some(parent) => {
                        set_separator_at_(&mut parent.children_, survivor_idx, survivor_max, cmp);
                    }
                    Option::None => return,
                }
            }

            current_ptr = parent_ptr;
            current_max = node_max_(parent_ptr);
            depth -= 1;
        } else {
            // 7. 索引节点：借位或合并。
            if current_len > 0 {
                if let Option::Some(left) = left_ptr {
                    if node_len_(left) > min {
                        let moved = match unsafe { node_mut_(left) }.as_index_mut_() {
                            Option::Some(left) => left.children_.try_remove_last(),
                            Option::None => unreachable!("左兄弟应为索引节点"),
                        };
                        if let Option::Some(pair) = moved {
                            match unsafe { node_mut_(current_ptr) }.as_index_mut_() {
                                Option::Some(current) => {
                                    let result = current.children_.try_insert_pair_by(pair, cmp);
                                    debug_assert!(result.is_succ());
                                }
                                Option::None => unreachable!("当前节点应为索引节点"),
                            }
                            let left_max = node_max_(left);
                            let current_max = node_max_(current_ptr);
                            if let (Option::Some(left_max), Option::Some(current_max)) =
                                (left_max, current_max)
                            {
                                match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                                    Option::Some(parent) => {
                                        set_separator_at_(
                                            &mut parent.children_,
                                            idx - 1,
                                            left_max,
                                            cmp,
                                        );
                                        set_separator_at_(
                                            &mut parent.children_,
                                            idx,
                                            current_max,
                                            cmp,
                                        );
                                    }
                                    Option::None => return,
                                }
                                let parent_max = node_max_(parent_ptr);
                                if let Option::Some(parent_max) = parent_max {
                                    refresh_separators_(path, depth - 1, parent_max, cmp);
                                }
                            }
                            return;
                        }
                    }
                } else if let Option::Some(right) = right_ptr {
                    if node_len_(right) > min {
                        let moved = match unsafe { node_mut_(right) }.as_index_mut_() {
                            Option::Some(right) => right.children_.try_remove_first(),
                            Option::None => unreachable!("右兄弟应为索引节点"),
                        };
                        if let Option::Some(pair) = moved {
                            match unsafe { node_mut_(current_ptr) }.as_index_mut_() {
                                Option::Some(current) => {
                                    let result = current.children_.try_insert_pair_by(pair, cmp);
                                    debug_assert!(result.is_succ());
                                }
                                Option::None => unreachable!("当前节点应为索引节点"),
                            }
                            let current_max = node_max_(current_ptr);
                            let right_max = node_max_(right);
                            if let (Option::Some(current_max), Option::Some(right_max)) =
                                (current_max, right_max)
                            {
                                match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                                    Option::Some(parent) => {
                                        set_separator_at_(
                                            &mut parent.children_,
                                            idx,
                                            current_max,
                                            cmp,
                                        );
                                        set_separator_at_(
                                            &mut parent.children_,
                                            idx + 1,
                                            right_max,
                                            cmp,
                                        );
                                    }
                                    Option::None => return,
                                }
                                let parent_max = node_max_(parent_ptr);
                                if let Option::Some(parent_max) = parent_max {
                                    refresh_separators_(path, depth - 1, parent_max, cmp);
                                }
                            }
                            return;
                        }
                    }
                }
            }

            let (survivor, victim, victim_idx) = match (left_ptr, right_ptr) {
                (Option::Some(left), _) => (left, current_ptr, idx),
                (Option::None, Option::Some(right)) => (current_ptr, right, idx + 1),
                (Option::None, Option::None) => return,
            };

            move_index_entries_(victim, survivor, cmp);
            let _ = nodes.remove(victim);

            match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                Option::Some(parent) => {
                    let _ = parent.children_.try_remove_at(victim_idx);
                }
                Option::None => return,
            }
            let survivor_idx = if victim_idx == idx { idx - 1 } else { idx };
            if let Option::Some(survivor_max) = node_max_(survivor) {
                match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                    Option::Some(parent) => {
                        set_separator_at_(&mut parent.children_, survivor_idx, survivor_max, cmp);
                    }
                    Option::None => return,
                }
            }

            current_ptr = parent_ptr;
            current_max = node_max_(parent_ptr);
            depth -= 1;
        }
    }
}

/// 删除查找结果。
enum InsertLocate<const M: usize, K, V> {
    /// 树为空。
    Empty,
    /// 已存在等价 key。
    Conflict(SlotPtr<(K, V)>),
    /// 不存在等价 key，给出目标叶子与 path。
    Missing(TreeNodePtr<M, K, V>, DescentPath<M, K, V>),
}

impl<const M: usize, C, K, V> BPlusTree<M, C, K, V>
where
    C: TrComparer<K>,
{
    /// 返回节点容量 `M`。
    pub const fn degree(&self) -> usize {
        M
    }

    /// 使用给定 comparer 创建一个空树。
    ///
    /// # Errors
    ///
    /// 使用默认 `SlabListConfig<DEFAULT_CHUNK_BYTES>` 创建数据与节点
    /// `SlabList`；若默认 Chunk 无法容纳至少一个元素，返回
    /// [`SlabListError`]。
    ///
    /// # Panics
    ///
    /// 当 `M < 2` 时 panic：B+ 树至少需要能容纳两个项的节点。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{BPlusTree, OrdComparer};
    ///
    /// let tree = BPlusTree::<4, _, i32, i32>::new_in(OrdComparer::new()).unwrap();
    /// assert!(tree.is_empty());
    /// ```
    pub fn new_in(comparer: C) -> Result<Self, SlabListError> {
        assert!(M >= 2, "B+ 树的节点容量 M 必须至少为 2");
        Ok(Self {
            len_: 0,
            comparer_: comparer,
            tree_root_: Option::None,
            first_leaf_: Option::None,
            last_leaf_: Option::None,
            data_arena_: SlabList::new(Global)?,
            node_arena_: SlabList::new(Global)?,
        })
    }

    /// 返回元素数量。
    #[inline]
    pub fn len(&self) -> usize {
        self.len_
    }

    /// 返回树是否为空。
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len_ == 0
    }

    /// 返回 comparer 的引用。
    #[inline]
    pub fn comparer(&self) -> &C {
        &self.comparer_
    }

    /// 返回最小 `(K, V)` 引用。
    pub fn first(&self) -> Option<(&K, &V)> {
        let leaf_ptr = self.first_leaf_?;
        // SAFETY: first_leaf_ 由树维护，指向存活叶子；&self 保证期间不变。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        let (data_ptr, _) = leaf.order_arr_.first()?;
        // SAFETY: 叶子中的句柄指向 data arena 中存活的数据；&self 保证期间不变。
        Option::Some(unsafe { data_pair_(*data_ptr) })
    }

    /// 返回最大 `(K, V)` 引用。
    pub fn last(&self) -> Option<(&K, &V)> {
        let leaf_ptr = self.last_leaf_?;
        // SAFETY: last_leaf_ 由树维护，指向存活叶子；&self 保证期间不变。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        let (data_ptr, _) = leaf.order_arr_.last()?;
        // SAFETY: 叶子中的句柄指向 data arena 中存活的数据；&self 保证期间不变。
        Option::Some(unsafe { data_pair_(*data_ptr) })
    }

    /// 使用存储的 comparer 查询与 `query` 等价的元素。
    ///
    /// `Q: Borrow<K>` 允许用 `&K` 或其他能借出 `K` 的类型查询。若需要
    /// 用完全不同的查询类型，请使用 [`Self::get_by`]。
    pub fn get<Q>(&self, query: Q) -> Option<(&K, &V)>
    where
        Q: Borrow<K>,
    {
        let query_ref = query.borrow();
        self.get_by(query_ref, &self.comparer_)
    }

    /// 使用存储的 comparer 查询并返回可变 value 引用。
    pub fn get_mut<Q>(&mut self, query: Q) -> Option<(&K, &mut V)>
    where
        Q: Borrow<K>,
    {
        let query_ref = query.borrow();
        let root = self.tree_root_?;
        let query_cmp = QuerySlotComparer::new_(&self.comparer_);
        let (leaf_ptr, _) = descend_to_leaf_(root, query_ref, &query_cmp);

        // SAFETY: 叶子指针由树维护；查询期间 &mut self 保证没有其他引用。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        let (_, data_ptr) = find_in_leaf_(leaf, query_ref, &query_cmp).ok()?;

        // SAFETY: data_ptr 指向存活数据；&mut self 保证独占访问。
        Option::Some(unsafe { data_pair_mut_(data_ptr) })
    }

    /// 使用异构 comparer 查询与 `query` 等价的元素。
    ///
    /// `cmp` 必须在 `K ↔ K` 上与树的存储 Comparer 一致。
    pub fn get_by<'f, Q, TyCmp>(&'f self, query: &Q, cmp: &TyCmp) -> Option<(&'f K, &'f V)>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        let root = self.tree_root_?;
        let query_cmp = QuerySlotComparer::new_(cmp);
        let (leaf_ptr, _) = descend_to_leaf_(root, query, &query_cmp);

        // SAFETY: 叶子指针由树维护；&self 保证期间结构不变。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        let (_, data_ptr) = find_in_leaf_(leaf, query, &query_cmp).ok()?;

        // SAFETY: data_ptr 指向存活数据；&self 的返回生命周期内不会被释放。
        Option::Some(unsafe { data_pair_(data_ptr) })
    }

    /// 使用异构 comparer 查询并返回可变 value 引用。
    pub fn get_mut_by<'f, Q, TyCmp>(
        &'f mut self,
        query: &Q,
        cmp: &TyCmp,
    ) -> Option<(&'f K, &'f mut V)>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        let root = self.tree_root_?;
        let query_cmp = QuerySlotComparer::new_(cmp);
        let (leaf_ptr, _) = descend_to_leaf_(root, query, &query_cmp);

        // SAFETY: 叶子指针由树维护；&mut self 保证没有其他引用。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        let (_, data_ptr) = find_in_leaf_(leaf, query, &query_cmp).ok()?;

        // SAFETY: data_ptr 指向存活数据；&mut self 保证独占访问。
        Option::Some(unsafe { data_pair_mut_(data_ptr) })
    }

    /// 查找插入位置。
    fn locate_for_insert_<Q, TyCmp>(&self, query: &Q, cmp: &TyCmp) -> InsertLocate<M, K, V>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        let Option::Some(root) = self.tree_root_ else {
            return InsertLocate::Empty;
        };

        let query_cmp = QuerySlotComparer::new_(cmp);
        let (leaf_ptr, path) = descend_to_leaf_(root, query, &query_cmp);

        // SAFETY: 叶子指针由树维护；&self 保证期间结构不变。
        let node = unsafe { node_ref_(leaf_ptr) };
        let Option::Some(leaf) = node.as_leaf_() else {
            return InsertLocate::Missing(leaf_ptr, path);
        };

        match find_in_leaf_(leaf, query, &query_cmp) {
            Ok((_, data_ptr)) => InsertLocate::Conflict(data_ptr),
            Err(_) => InsertLocate::Missing(leaf_ptr, path),
        }
    }

    /// 在空树中插入第一个元素。
    fn insert_first_entry_(&mut self, pair: (K, V)) -> SlotPtr<(K, V)> {
        let data_ptr = self.data_arena_.insert(pair);
        let node_cmp = SlotPairKeyComparer::new_(&self.comparer_);

        let mut leaf = DataLeaf::new_();
        let result = leaf
            .order_arr_
            .try_insert_pair_by((data_ptr, ()), &node_cmp);
        debug_assert!(result.is_succ());

        let leaf_ptr = self.node_arena_.insert(TreeNode::Leaf(leaf));
        self.tree_root_ = Option::Some(leaf_ptr);
        self.first_leaf_ = Option::Some(leaf_ptr);
        self.last_leaf_ = Option::Some(leaf_ptr);
        self.len_ = 1;
        data_ptr
    }

    /// 把已写入 data arena 的新数据插入树结构。
    fn insert_data_after_locate_(
        &mut self,
        data_ptr: SlotPtr<(K, V)>,
        leaf_ptr: TreeNodePtr<M, K, V>,
        path: DescentPath<M, K, V>,
    ) {
        let node_cmp = SlotPairKeyComparer::new_(&self.comparer_);
        insert_data_into_tree_(
            &mut self.node_arena_,
            &mut self.tree_root_,
            &mut self.last_leaf_,
            leaf_ptr,
            path,
            data_ptr,
            &node_cmp,
        );
        self.len_ += 1;
    }

    /// 使用异构 comparer 插入一个新元素。
    ///
    /// `factory` 只在确认不存在等价 key 后调用一次；插入成功时返回新元素的
    /// `(&K, &mut V)`。若已存在等价 key，返回 [`ConflictInfo`]，且
    /// `conflict` 为 `None`（factory 未被调用）。
    ///
    /// `factory` 返回的 `K` 必须与 `query` 在 `cmp` 下比较等价，并且 `cmp` 在
    /// `K ↔ K` 上必须与树的存储 Comparer 一致，否则树无法保证有序性。
    ///
    /// # Errors
    ///
    /// 当树中已经存在与 `query` 等价的 key 时返回 [`ConflictInfo`]。
    pub fn try_insert_by<'f, Q, TyCmp>(
        &'f mut self,
        query: &Q,
        cmp: &TyCmp,
        factory: impl FnOnce(&Q) -> (K, V),
    ) -> Result<(&'f K, &'f mut V), ConflictInfo<'f, K, V>>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        match self.locate_for_insert_(query, cmp) {
            InsertLocate::Conflict(data_ptr) => {
                // SAFETY: data_ptr 指向树中仍存活的数据；&mut self 保证独占访问。
                Err(ConflictInfo {
                    existing_: unsafe { data_pair_mut_(data_ptr) },
                    conflict_: Option::None,
                })
            }
            InsertLocate::Empty => {
                let data_ptr = self.insert_first_entry_(factory(query));
                // SAFETY: 刚插入的数据存活；&mut self 保证独占访问。
                Ok(unsafe { data_pair_mut_(data_ptr) })
            }
            InsertLocate::Missing(leaf_ptr, path) => {
                let data_ptr = self.data_arena_.insert(factory(query));
                self.insert_data_after_locate_(data_ptr, leaf_ptr, path);
                // SAFETY: 刚插入的数据存活；&mut self 保证独占访问。
                Ok(unsafe { data_pair_mut_(data_ptr) })
            }
        }
    }

    /// 使用存储的 comparer 插入一个已构造好的 `(K, V)`。
    ///
    /// 插入成功返回 `(&K, &mut V)`；若已存在等价 key，返回
    /// [`ConflictInfo`]，其 `conflict` 为 `Some((K, V))`，包含调用方传入的
    /// 键值对。
    ///
    /// # Errors
    ///
    /// 当树中已经存在与 `key` 等价的 key 时返回 [`ConflictInfo`]。
    pub fn try_insert<'f>(
        &'f mut self,
        key: K,
        value: V,
    ) -> Result<(&'f K, &'f mut V), ConflictInfo<'f, K, V>> {
        let pair = (key, value);

        let located = self.locate_for_insert_(&pair.0, &self.comparer_);

        match located {
            InsertLocate::Conflict(data_ptr) => {
                // SAFETY: data_ptr 指向树中仍存活的数据；&mut self 保证独占访问。
                Err(ConflictInfo {
                    existing_: unsafe { data_pair_mut_(data_ptr) },
                    conflict_: Option::Some(pair),
                })
            }
            InsertLocate::Empty => {
                let data_ptr = self.insert_first_entry_(pair);
                // SAFETY: 刚插入的数据存活；&mut self 保证独占访问。
                Ok(unsafe { data_pair_mut_(data_ptr) })
            }
            InsertLocate::Missing(leaf_ptr, path) => {
                let data_ptr = self.data_arena_.insert(pair);
                self.insert_data_after_locate_(data_ptr, leaf_ptr, path);
                // SAFETY: 刚插入的数据存活；&mut self 保证独占访问。
                Ok(unsafe { data_pair_mut_(data_ptr) })
            }
        }
    }

    /// 插入 `(K, V)`；若 key 已存在则覆盖 value。
    ///
    /// 返回被覆盖的旧 value；不覆盖时返回 `None`。旧 key 保持不变，因为
    /// 索引中的 separator 仍然引用它。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{BPlusTree, OrdComparer};
    ///
    /// let mut tree = BPlusTree::<4, _, i32, &str>::new_in(OrdComparer::new()).unwrap();
    /// assert_eq!(tree.insert(1, "one"), None);
    /// assert_eq!(tree.insert(1, "uno"), Some("one"));
    /// assert_eq!(tree.get(1), Some((&1, &"uno")));
    /// ```
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        match self.try_insert(key, value) {
            Ok(_) => Option::None,
            Err(info) => {
                let ((_, existing_value), conflict) = info.into_parts();
                match conflict {
                    Option::Some((_, value)) => Option::Some(mem::replace(existing_value, value)),
                    Option::None => Option::None,
                }
            }
        }
    }

    /// 删除与 `query` 等价的元素，返回被删除的 value。
    pub fn remove<Q>(&mut self, query: Q) -> Option<V>
    where
        Q: Borrow<K>,
    {
        self.remove_entry(query).map(|(_, value)| value)
    }

    /// 删除与 `query` 等价的元素，返回完整的 `(K, V)`。
    pub fn remove_entry<Q>(&mut self, query: Q) -> Option<(K, V)>
    where
        Q: Borrow<K>,
    {
        let query_ref = query.borrow();
        remove_entry_by_(
            &mut self.data_arena_,
            &mut self.node_arena_,
            &mut self.tree_root_,
            &mut self.first_leaf_,
            &mut self.last_leaf_,
            &mut self.len_,
            query_ref,
            &self.comparer_,
            &self.comparer_,
        )
    }

    /// 使用异构 comparer 删除与 `query` 等价的元素，返回 value。
    pub fn remove_by<Q, TyCmp>(&mut self, query: &Q, cmp: &TyCmp) -> Option<V>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        self.remove_entry_by(query, cmp).map(|(_, value)| value)
    }

    /// 使用异构 comparer 删除与 `query` 等价的元素，返回完整 `(K, V)`。
    ///
    /// `cmp` 必须在 `K ↔ K` 上与树的存储 Comparer 一致。
    pub fn remove_entry_by<Q, TyCmp>(&mut self, query: &Q, cmp: &TyCmp) -> Option<(K, V)>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        remove_entry_by_(
            &mut self.data_arena_,
            &mut self.node_arena_,
            &mut self.tree_root_,
            &mut self.first_leaf_,
            &mut self.last_leaf_,
            &mut self.len_,
            query,
            cmp,
            &self.comparer_,
        )
    }

    /// 返回按 key 升序迭代的迭代器。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{BPlusTree, OrdComparer};
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new_in(OrdComparer::new()).unwrap();
    /// tree.insert(2, 20);
    /// tree.insert(1, 10);
    /// let keys: Vec<i32> = tree.iter().map(|(key, _)| *key).collect();
    /// assert_eq!(keys, vec![1, 2]);
    /// ```
    pub fn iter(&self) -> TreeIter<'_, M, K, V> {
        TreeIter {
            next_leaf_: self.first_leaf_,
            index_: 0,
            remaining_: self.len_,
            _mark_: PhantomData,
        }
    }

    /// 从已有 `(K, V)` 集合构建树。
    ///
    /// 重复 key 按 Rust `FromIterator` 的习惯保留最后一次出现的 value；
    /// 已有 key 本身保持不变，以维持 separator 对数据槽位的引用。
    ///
    /// # Errors
    ///
    /// 与 [`Self::new_in`] 相同。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{BPlusTree, OrdComparer};
    ///
    /// let tree = BPlusTree::<4, _, i32, i32>::from_iter_by(
    ///     OrdComparer::new(),
    ///     vec![(2, 20), (1, 10)],
    /// )
    /// .unwrap();
    /// assert_eq!(tree.len(), 2);
    /// assert_eq!(tree.first(), Some((&1, &10)));
    /// ```
    pub fn from_iter_by(
        comparer: C,
        iter: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Self, SlabListError> {
        let mut tree = Self::new_in(comparer)?;
        tree.extend(iter);
        Ok(tree)
    }
}

/// 删除核心逻辑。
///
/// `query_cmp` 用于与查询对象比较，`key_cmp` 用于节点内部排序。
#[allow(clippy::too_many_arguments)]
fn remove_entry_by_<const M: usize, K, V, Q, TyCmp, C>(
    data_arena: &mut SlabList<(K, V)>,
    node_arena: &mut SlabList<TreeNode<M, K, V>>,
    tree_root: &mut Option<TreeNodePtr<M, K, V>>,
    first_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    last_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    len: &mut usize,
    query: &Q,
    query_cmp: &TyCmp,
    key_cmp: &C,
) -> Option<(K, V)>
where
    Q: ?Sized,
    TyCmp: TrComparer<K, Q>,
    C: TrComparer<K>,
{
    let root = (*tree_root)?;
    let query_cmp = QuerySlotComparer::new_(query_cmp);
    let (leaf_ptr, path) = descend_to_leaf_(root, query, &query_cmp);

    let (pos, data_ptr) = {
        // SAFETY: 叶子指针由树维护；调用期间 &mut 字段保证没有其他别名。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        find_in_leaf_(leaf, query, &query_cmp).ok()?
    };

    let removed = match unsafe { node_mut_(leaf_ptr) }.as_leaf_mut_() {
        Option::Some(leaf) => leaf.order_arr_.try_remove_at(pos),
        Option::None => Option::None,
    };
    let (removed_ptr, ()) = removed?;
    debug_assert_eq!(removed_ptr, data_ptr);

    let leaf_max = node_max_(leaf_ptr);
    let node_cmp = SlotPairKeyComparer::new_(key_cmp);
    rebalance_after_remove_(
        node_arena,
        tree_root,
        first_leaf,
        last_leaf,
        &path,
        leaf_ptr,
        leaf_max,
        path.len(),
        &node_cmp,
    );

    *len -= 1;
    Option::Some(data_arena.remove(data_ptr))
}

/// 创建树时保存的字段。
pub struct BPlusTree<const M: usize, C, K, V = ()>
where
    C: TrComparer<K>,
{
    len_: usize,
    comparer_: C,
    tree_root_: Option<TreeNodePtr<M, K, V>>,
    first_leaf_: Option<TreeNodePtr<M, K, V>>,
    last_leaf_: Option<TreeNodePtr<M, K, V>>,
    data_arena_: SlabList<(K, V)>,
    node_arena_: SlabList<TreeNode<M, K, V>>,
}

/// 插入冲突信息。
///
/// - [`BPlusTree::try_insert_by`] 在冲突时不调用 factory，因此
///   `conflict` 为 `None`；
/// - [`BPlusTree::try_insert`] 已经持有调用方传入的键值对，因此
///   `conflict` 为 `Some((K, V))`。
#[derive(Debug)]
pub struct ConflictInfo<'a, K, V = ()> {
    /// 树中已存在的键值引用。
    existing_: (&'a K, &'a mut V),
    /// 未能插入的键值对（如果调用方已经构造了它）。
    conflict_: Option<(K, V)>,
}

impl<'a, K, V> ConflictInfo<'a, K, V> {
    /// 返回树中已存在的 key 与 value 引用。
    pub fn existing(&self) -> (&K, &V) {
        (self.existing_.0, &*self.existing_.1)
    }

    /// 返回未能插入的键值对引用。
    pub fn conflict(&self) -> Option<&(K, V)> {
        self.conflict_.as_ref()
    }

    /// 拆分为已存在引用与未插入的键值对。
    pub fn into_parts(self) -> ((&'a K, &'a mut V), Option<(K, V)>) {
        (self.existing_, self.conflict_)
    }

    /// 返回树中已存在的 key 与可变 value 引用。
    pub fn into_existing(self) -> (&'a K, &'a mut V) {
        self.existing_
    }

    /// 返回未能插入的键值对。
    pub fn into_conflict(self) -> Option<(K, V)> {
        self.conflict_
    }
}

impl<const M: usize, C, K, V> Extend<(K, V)> for BPlusTree<M, C, K, V>
where
    C: TrComparer<K>,
{
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (key, value) in iter {
            let _ = self.insert(key, value);
        }
    }
}

/// 使用 `C::default()` 作为 comparer，从 `(K, V)` 迭代器构建树。
///
/// 重复 key 保留最后出现的 value。若默认配置无法容纳 `(K, V)` 或树节点，
/// 本实现会 panic；需要可失败版本时使用 [`BPlusTree::from_iter_by`]。
impl<const M: usize, C, K, V> FromIterator<(K, V)> for BPlusTree<M, C, K, V>
where
    C: TrComparer<K> + Default,
{
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        match Self::from_iter_by(C::default(), iter) {
            Ok(tree) => tree,
            Err(err) => panic!("使用默认 Comparer 构建 BPlusTree 失败: {err}"),
        }
    }
}

/// 按 key 升序迭代 [`BPlusTree`]。
///
/// 迭代器不持有树的引用计数；它借用树，因此不能在迭代期间修改树。
///
/// # Examples
///
/// ```
/// use bplustree_cmp::{BPlusTree, OrdComparer};
///
/// let mut tree = BPlusTree::<4, _, i32, i32>::new_in(OrdComparer::new()).unwrap();
/// tree.insert(1, 10);
///
/// let mut iter = tree.iter();
/// assert_eq!(iter.len(), 1);
/// assert_eq!(iter.next(), Some((&1, &10)));
/// assert_eq!(iter.next(), None);
/// ```
pub struct TreeIter<'a, const M: usize, K, V = ()> {
    next_leaf_: Option<TreeNodePtr<M, K, V>>,
    index_: usize,
    remaining_: usize,
    _mark_: PhantomData<&'a (K, V)>,
}

impl<'a, const M: usize, K, V> Iterator for TreeIter<'a, M, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let leaf_ptr = self.next_leaf_?;

            // SAFETY: 迭代器借用树，叶子和数据在 'a 内保持存活。
            let node = unsafe { node_ref_(leaf_ptr) };
            let Some(leaf) = node.as_leaf_() else {
                self.next_leaf_ = Option::None;
                return Option::None;
            };

            if self.index_ < leaf.order_arr_.len() {
                match leaf.order_arr_.get_ref_at(self.index_) {
                    Ok((data_ptr, _)) => {
                        self.index_ += 1;
                        self.remaining_ -= 1;
                        // SAFETY: data_ptr 指向 'a 内仍存活的数据。
                        return Option::Some(unsafe { data_pair_(*data_ptr) });
                    }
                    Err(_) => {
                        self.index_ = 0;
                        self.next_leaf_ = leaf.next_sibl_;
                        continue;
                    }
                }
            }

            self.index_ = 0;
            self.next_leaf_ = leaf.next_sibl_;
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining_, Option::Some(self.remaining_))
    }
}

impl<const M: usize, K, V> ExactSizeIterator for TreeIter<'_, M, K, V> {}

impl<const M: usize, K, V> FusedIterator for TreeIter<'_, M, K, V> {}

/// 对 `&BPlusTree` 的迭代器。
impl<'a, const M: usize, C, K, V> IntoIterator for &'a BPlusTree<M, C, K, V>
where
    C: TrComparer<K>,
{
    type Item = (&'a K, &'a V);
    type IntoIter = TreeIter<'a, M, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
impl<const M: usize, C, K, V> BPlusTree<M, C, K, V>
where
    C: TrComparer<K>,
{
    /// 检查树结构与 separator 不变量。
    fn check_invariants_(&self) {
        let cmp = SlotPairKeyComparer::new_(&self.comparer_);
        let Option::Some(root) = self.tree_root_ else {
            assert_eq!(self.len_, 0);
            assert!(self.first_leaf_.is_none());
            assert!(self.last_leaf_.is_none());
            return;
        };

        let mut leaves = alloc::vec::Vec::new();
        let (count, _max, _height) = self.check_node_(root, true, &mut leaves, &cmp);

        assert_eq!(count, self.len_);
        assert_eq!(leaves.first().copied(), self.first_leaf_);
        assert_eq!(leaves.last().copied(), self.last_leaf_);

        for pair in leaves.windows(2) {
            let left = pair[0];
            let right = pair[1];
            let left_leaf = unsafe { node_ref_(left) };
            let right_leaf = unsafe { node_ref_(right) };
            let left_leaf = left_leaf.as_leaf_().expect("叶子链应只包含叶子");
            let right_leaf = right_leaf.as_leaf_().expect("叶子链应只包含叶子");
            assert_eq!(left_leaf.next_sibl_, Option::Some(right));
            assert_eq!(right_leaf.prev_sibl_, Option::Some(left));
        }

        if let Option::Some(first) = leaves.first().copied() {
            let leaf = unsafe { node_ref_(first) };
            let leaf = leaf.as_leaf_().expect("首叶应为叶子");
            assert!(leaf.prev_sibl_.is_none());
        }
        if let Option::Some(last) = leaves.last().copied() {
            let leaf = unsafe { node_ref_(last) };
            let leaf = leaf.as_leaf_().expect("尾叶应为叶子");
            assert!(leaf.next_sibl_.is_none());
        }
    }

    fn check_node_(
        &self,
        ptr: TreeNodePtr<M, K, V>,
        is_root: bool,
        leaves: &mut alloc::vec::Vec<TreeNodePtr<M, K, V>>,
        cmp: &SlotPairKeyComparer<'_, K, V, C>,
    ) -> (usize, Option<SlotPtr<(K, V)>>, usize) {
        let node = unsafe { node_ref_(ptr) };
        match node {
            TreeNode::Leaf(leaf) => {
                let len = leaf.len_();
                assert!(len > 0);
                if !is_root {
                    assert!(len >= min_occupancy_(M));
                }
                for i in 1..len {
                    let (prev, _) = leaf.order_arr_.get_ref_at(i - 1).expect("下标应有效");
                    let (curr, _) = leaf.order_arr_.get_ref_at(i).expect("下标应有效");
                    assert_eq!(cmp.compare(prev, curr), core::cmp::Ordering::Less);
                }
                leaves.push(ptr);
                let max = leaf.order_arr_.last().map(|(key, _)| *key);
                (len, max, 1)
            }
            TreeNode::Index(index) => {
                let len = index.len_();
                assert!(len > 0);
                if is_root {
                    assert!(len >= 2);
                } else {
                    assert!(len >= min_occupancy_(M));
                }

                let mut total = 0usize;
                let mut height = Option::None;
                let mut prev_key: Option<SlotPtr<(K, V)>> = Option::None;

                for i in 0..len {
                    let (sep, child) = index.children_.get_ref_at(i).expect("下标应有效");
                    if let Option::Some(prev_key) = prev_key {
                        assert_eq!(cmp.compare(&prev_key, sep), core::cmp::Ordering::Less);
                    }
                    prev_key = Option::Some(*sep);

                    let (child_count, child_max, child_height) =
                        self.check_node_(*child, false, leaves, cmp);
                    assert_eq!(child_max, Option::Some(*sep));

                    total += child_count;
                    match height {
                        Option::None => height = Option::Some(child_height),
                        Option::Some(height) => assert_eq!(height, child_height),
                    }
                }

                let max = index.children_.last().map(|(key, _)| *key);
                (total, max, height.expect("非空索引应有孩子") + 1)
            }
        }
    }
}

#[cfg(test)]
mod tests_tree_ {
    use std::{
        collections::BTreeMap,
        string::{String, ToString},
        vec,
        vec::Vec,
    };

    use core::cmp::Ordering;

    use super::*;
    use crate::comparer_::OrdComparer;

    /// 测试目标：验证空树的查询、首尾与迭代行为。
    /// Test goal: Verify query, first/last, and iteration behavior of an empty tree.
    ///
    /// 测试手段：创建空树后调用 get / first / last / remove / iter。
    /// Test method: Create an empty tree and call get / first / last / remove / iter.
    ///
    /// 判定标准：查询与删除返回 None，首尾为 None，迭代器立即结束，len 为 0。
    /// Assertions: Query and removal return None, first/last are None, the iterator
    /// ends immediately, and len is zero.
    #[test]
    fn test_empty_tree() {
        let mut tree = BPlusTree::<4, _, i32, i32>::new_in(OrdComparer::new()).unwrap();

        assert_eq!(tree.len(), 0);
        assert!(tree.is_empty());
        assert_eq!(tree.first(), None);
        assert_eq!(tree.last(), None);
        assert_eq!(tree.get(1), None);
        assert_eq!(tree.remove(1), None);
        assert_eq!(tree.iter().count(), 0);
        tree.check_invariants_();
    }

    /// 测试目标：验证升序/降序插入后的查询、首尾与有序迭代。
    /// Test goal: Verify query, first/last, and ordered iteration after ascending and
    /// descending insertion.
    ///
    /// 测试手段：分别插入 0..64 和 64..0，检查每个 key 的查询结果以及迭代顺序。
    /// Test method: Insert 0..64 and 64..0 separately, checking every key lookup and
    /// the iteration order.
    ///
    /// 判定标准：所有查询命中正确 value，first/last 正确，迭代严格升序，结构不变量成立。
    /// Assertions: Every lookup returns the expected value, first/last are correct,
    /// iteration is strictly ascending, and structural invariants hold.
    #[test]
    fn test_insert_get_and_order() {
        for reversed in [false, true] {
            let mut tree = BPlusTree::<4, _, i32, i32>::new_in(OrdComparer::new()).unwrap();
            let keys: Vec<i32> = if reversed {
                (0..64).rev().collect()
            } else {
                (0..64).collect()
            };

            for key in keys {
                assert_eq!(tree.insert(key, key * 10), None);
            }

            assert_eq!(tree.len(), 64);
            assert_eq!(tree.first(), Some((&0, &0)));
            assert_eq!(tree.last(), Some((&63, &630)));

            for key in 0..64 {
                assert_eq!(tree.get(key), Some((&key, &(key * 10))));
            }

            let collected: Vec<i32> = tree.iter().map(|(key, _)| *key).collect();
            assert_eq!(collected, (0..64).collect::<Vec<_>>());
            tree.check_invariants_();
        }
    }

    /// 测试目标：验证重复 key 的冲突信息与覆盖语义。
    /// Test goal: Verify conflict information and overwrite semantics for duplicate keys.
    ///
    /// 测试手段：先 try_insert 插入一次，再用相同 key 调用 try_insert 与 insert。
    /// Test method: Insert once with try_insert, then call try_insert and insert again
    /// with the same key.
    ///
    /// 判定标准：第二次 try_insert 返回 ConflictInfo，conflict 包含传入的 pair；
    /// insert 返回旧 value，长度不变，key 仍为原 key。
    /// Assertions: The second try_insert returns ConflictInfo whose conflict holds the
    /// supplied pair; insert returns the old value, the length is unchanged, and the
    /// stored key stays the original one.
    #[test]
    fn test_duplicate_insert() {
        let mut tree = BPlusTree::<4, _, i32, &'static str>::new_in(OrdComparer::new()).unwrap();

        assert!(tree.try_insert(1, "one").is_ok());

        let conflict = tree.try_insert(1, "uno").unwrap_err();
        assert_eq!(conflict.existing(), (&1, &"one"));
        assert_eq!(conflict.conflict(), Some(&(1, "uno")));
        assert_eq!(tree.len(), 1);

        assert_eq!(tree.insert(1, "eins"), Some("one"));
        assert_eq!(tree.get(1), Some((&1, &"eins")));
        assert_eq!(tree.len(), 1);
        tree.check_invariants_();
    }

    /// 测试目标：验证删除后树仍然保持结构与查询正确性。
    /// Test goal: Verify that the tree keeps its structure and lookup correctness
    /// after removals.
    ///
    /// 测试手段：插入 0..128 后删除其中的偶数，再删除剩余部分。
    /// Test method: Insert 0..128, remove all even keys, then remove the remaining
    /// keys.
    ///
    /// 判定标准：删除返回正确 value，未删除 key 仍可查询，最终树为空，且每步结构
    /// 不变量成立。
    /// Assertions: Removal returns the expected value, remaining keys are still
    /// reachable, the tree finally becomes empty, and invariants hold at each step.
    #[test]
    fn test_remove_half_and_all() {
        let mut tree = BPlusTree::<4, _, i32, i32>::new_in(OrdComparer::new()).unwrap();
        for key in 0..128 {
            let _ = tree.insert(key, key);
        }

        for key in (0..128).step_by(2) {
            assert_eq!(tree.remove(key), Some(key));
            tree.check_invariants_();
        }

        for key in (1..128).step_by(2) {
            assert_eq!(tree.get(key), Some((&key, &key)));
        }

        for key in (1..128).step_by(2) {
            assert_eq!(tree.remove(key), Some(key));
            tree.check_invariants_();
        }

        assert!(tree.is_empty());
        assert_eq!(tree.first(), None);
        assert_eq!(tree.last(), None);
        tree.check_invariants_();
    }

    /// 测试目标：用随机操作与 `BTreeMap` 模型对拍，检查增删查与结构不变量。
    /// Test goal: Differential-test random operations against a `BTreeMap` model,
    /// covering insertion, removal, lookup, and structural invariants.
    ///
    /// 测试手段：使用固定种子的 xorshift 生成 2000 次插入/删除/查询，key 空间为
    /// 0..512；每次操作后比较长度、查询结果，并周期性检查完整迭代与不变量。
    /// Test method: Use a fixed-seed xorshift to generate 2000 operations over the key
    /// space 0..512, comparing lengths and lookups after each operation, and checking
    /// full iteration and invariants periodically.
    ///
    /// 判定标准：树与模型的长度、查询、删除返回值完全一致；完整迭代顺序一致；
    /// 结构不变量始终成立。
    /// Assertions: Lengths, lookups, and removal results match the model; full
    /// iteration order matches; structural invariants always hold.
    #[test]
    fn test_random_against_btreemap() {
        let degrees: &[usize] = if cfg!(miri) {
            &[2, 4]
        } else {
            &[2, 3, 4, 5, 6]
        };
        for &degree in degrees {
            random_model_test_(degree);
        }
    }

    fn random_model_test_(degree: usize) {
        match degree {
            2 => random_model_for_::<2>(),
            3 => random_model_for_::<3>(),
            4 => random_model_for_::<4>(),
            5 => random_model_for_::<5>(),
            6 => random_model_for_::<6>(),
            _ => unreachable!(),
        }
    }

    fn random_model_for_<const M: usize>() {
        let mut tree = BPlusTree::<M, _, i32, i32>::new_in(OrdComparer::new()).unwrap();
        let mut model = BTreeMap::<i32, i32>::new();
        let mut state = 0x1234_5678_9abc_def0u64;

        let steps = if cfg!(miri) { 40 } else { 2000 };
        for step in 0..steps {
            let key = (next_rand_(&mut state) % 512) as i32;
            match next_rand_(&mut state) % 3 {
                0 => {
                    let value = key * 7 + step;
                    let old = tree.insert(key, value);
                    let expected = model.insert(key, value);
                    assert_eq!(old, expected);
                }
                1 => {
                    let actual = tree.remove(key);
                    let expected = model.remove(&key);
                    assert_eq!(actual, expected);
                }
                _ => {
                    let actual = tree.get(key).map(|(key, value)| (*key, *value));
                    let expected = model.get(&key).map(|value| (key, *value));
                    assert_eq!(actual, expected);
                }
            }

            assert_eq!(tree.len(), model.len());
            assert_eq!(
                tree.first().map(|(key, value)| (*key, *value)),
                model.first_key_value().map(|(key, value)| (*key, *value))
            );
            assert_eq!(
                tree.last().map(|(key, value)| (*key, *value)),
                model.last_key_value().map(|(key, value)| (*key, *value))
            );
            tree.check_invariants_();

            if step % 97 == 0 {
                let actual: Vec<(i32, i32)> =
                    tree.iter().map(|(key, value)| (*key, *value)).collect();
                let expected: Vec<(i32, i32)> =
                    model.iter().map(|(key, value)| (*key, *value)).collect();
                assert_eq!(actual, expected);
            }
        }
    }

    fn next_rand_(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[derive(Debug)]
    struct User {
        id: u64,
        name: String,
    }

    struct UserIdCmp;

    impl TrComparer<User> for UserIdCmp {
        fn compare(&self, a: &User, b: &User) -> Ordering {
            a.id.cmp(&b.id)
        }
    }

    struct UserIdQueryCmp;

    impl TrComparer<User, u64> for UserIdQueryCmp {
        fn compare(&self, a: &User, b: &u64) -> Ordering {
            a.id.cmp(b)
        }
    }

    /// 测试目标：验证异构查询、factory 构造与按 `u64` 删除。
    /// Test goal: Verify heterogeneous queries, factory construction, and removal by
    /// a `u64` query.
    ///
    /// 测试手段：树中保存 `User`，按 `User.id` 排序；用 `u64` hint 与
    /// `TrComparer<User, u64>` 插入、查询和删除。
    /// Test method: Store `User` ordered by `User.id`; insert, query, and remove using
    /// a `u64` hint and `TrComparer<User, u64>`.
    ///
    /// 判定标准：插入后能按 id 查到对应 User；按 id 删除返回正确 User；树中不需要
    /// 构造临时 User 即可完成查询。
    /// Assertions: Inserted users are found by id; removal by id returns the correct
    /// user; queries work without constructing a temporary `User`.
    #[test]
    fn test_heterogeneous_query() {
        let mut tree = BPlusTree::<4, _, User, u64>::new_in(UserIdCmp).expect("默认配置应当有效");
        let query_cmp = UserIdQueryCmp;

        for id in 0..64u64 {
            let result = tree.try_insert_by(&id, &query_cmp, |id| {
                (
                    User {
                        id: *id,
                        name: "user".to_string(),
                    },
                    *id * 10,
                )
            });
            assert!(result.is_ok());
        }

        assert_eq!(tree.len(), 64);
        for id in 0..64u64 {
            let (user, value) = tree.get_by(&id, &query_cmp).expect("按 id 应能查到 User");
            assert_eq!(user.id, id);
            assert_eq!(user.name, "user");
            assert_eq!(*value, id * 10);
        }

        let removed = tree
            .remove_entry_by(&32u64, &query_cmp)
            .expect("按 id 应能删除 User");
        assert_eq!(removed.0.id, 32);
        assert_eq!(removed.1, 320);
        assert!(tree.get_by(&32u64, &query_cmp).is_none());
        tree.check_invariants_();
    }

    struct RevCmp;

    impl TrComparer<i32> for RevCmp {
        fn compare(&self, a: &i32, b: &i32) -> Ordering {
            b.cmp(a)
        }
    }

    /// 测试目标：验证排序关系完全由 Comparer 决定。
    /// Test goal: Verify that ordering is defined entirely by the comparer.
    ///
    /// 测试手段：使用反向 comparer 插入 0..32。
    /// Test method: Insert 0..32 with a reverse comparer.
    ///
    /// 判定标准：first 为最大值、last 为最小值，迭代严格降序，查找仍然正确。
    /// Assertions: first is the maximum, last is the minimum, iteration is strictly
    /// descending, and lookups still work.
    #[test]
    fn test_reverse_comparer() {
        let mut tree = BPlusTree::<4, _, i32, i32>::new_in(RevCmp).unwrap();
        for key in 0..32 {
            let _ = tree.insert(key, key);
        }

        assert_eq!(tree.first().map(|(key, _)| *key), Some(31));
        assert_eq!(tree.last().map(|(key, _)| *key), Some(0));
        let keys: Vec<i32> = tree.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, (0..32).rev().collect::<Vec<_>>());
        assert_eq!(tree.get(7), Some((&7, &7)));
        tree.check_invariants_();
    }

    #[derive(Debug)]
    struct NoCloneKey {
        id: u64,
    }

    struct NoCloneCmp;

    impl TrComparer<NoCloneKey> for NoCloneCmp {
        fn compare(&self, a: &NoCloneKey, b: &NoCloneKey) -> Ordering {
            a.id.cmp(&b.id)
        }
    }

    struct NoCloneQueryCmp;

    impl TrComparer<NoCloneKey, u64> for NoCloneQueryCmp {
        fn compare(&self, a: &NoCloneKey, b: &u64) -> Ordering {
            a.id.cmp(b)
        }
    }

    /// 测试目标：验证 Key 不需要 `Clone`，且分裂不会复制 Key 的地址。
    /// Test goal: Verify that keys do not need `Clone` and that splits never copy key
    /// addresses.
    ///
    /// 测试手段：使用没有实现 `Clone` 的 `NoCloneKey`，插入 256 个元素；记录前 32
    /// 个 key 的地址，继续插入触发多层分裂后再检查地址。
    /// Test method: Use `NoCloneKey` (which does not implement `Clone`), insert 256
    /// entries, record the addresses of the first 32 keys, and check them again after
    /// further inserts trigger multiple splits.
    ///
    /// 判定标准：代码可编译（证明无 Clone 要求）；记录的 key 地址完全不变。
    /// Assertions: The code compiles (proving no `Clone` requirement), and recorded
    /// key addresses remain identical.
    #[test]
    fn test_no_clone_key_and_stable_addresses() {
        let mut tree =
            BPlusTree::<4, _, NoCloneKey, u64>::new_in(NoCloneCmp).expect("默认配置应当有效");
        let query_cmp = NoCloneQueryCmp;

        for id in 0..256u64 {
            let result = tree.try_insert_by(&id, &query_cmp, |id| (NoCloneKey { id: *id }, id * 3));
            assert!(result.is_ok());
        }

        let mut addresses = Vec::new();
        for id in 0..32u64 {
            let (key, _) = tree.get_by(&id, &query_cmp).expect("key 应存在");
            addresses.push((id, key as *const NoCloneKey));
        }

        for id in 256..1024u64 {
            let result = tree.try_insert_by(&id, &query_cmp, |id| (NoCloneKey { id: *id }, id * 3));
            assert!(result.is_ok());
        }

        for (id, address) in addresses {
            let (key, value) = tree.get_by(&id, &query_cmp).expect("key 应存在");
            assert_eq!(key as *const NoCloneKey, address);
            assert_eq!(*value, id * 3);
        }
        tree.check_invariants_();
    }

    /// 测试目标：验证 `from_iter_by` 与 `Extend` 的构建语义。
    /// Test goal: Verify the construction semantics of `from_iter_by` and `Extend`.
    ///
    /// 测试手段：用 `from_iter_by` 从含重复 key 的集合构建树，再 `extend` 新元素。
    /// Test method: Build a tree with `from_iter_by` from a collection containing
    /// duplicate keys, then `extend` it with new elements.
    ///
    /// 判定标准：重复 key 保留最后 value；extend 后长度和查询正确；迭代有序。
    /// Assertions: Duplicate keys keep the last value; length and lookups are correct
    /// after extend; iteration is ordered.
    #[test]
    fn test_from_iter_and_extend() {
        let items = vec![(3, "three"), (1, "one"), (2, "two"), (1, "uno")];
        let mut tree =
            BPlusTree::<4, _, i32, &'static str>::from_iter_by(OrdComparer::new(), items)
                .expect("默认配置应当有效");

        assert_eq!(tree.len(), 3);
        assert_eq!(tree.get(1), Some((&1, &"uno")));

        tree.extend(vec![(4, "four"), (0, "zero")]);
        assert_eq!(tree.len(), 5);
        let keys: Vec<i32> = tree.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, vec![0, 1, 2, 3, 4]);
        tree.check_invariants_();
    }

    /// 测试目标：验证可变查询、冲突更新与 insert 的覆盖语义。
    /// Test goal: Verify mutable lookup, conflict updates, and insert overwrite
    /// semantics.
    ///
    /// 测试手段：插入三个元素，通过 `get_mut` 修改 value，通过 `ConflictInfo`
    /// 修改已有 value，再用 `insert` 覆盖并检查返回值。
    /// Test method: Insert three elements, mutate a value through `get_mut`, mutate
    /// an existing value through `ConflictInfo`, then overwrite with `insert` and
    /// check the returned old value.
    ///
    /// 判定标准：三种修改都生效，长度不变，旧 value 返回正确。
    /// Assertions: All three mutations take effect, the length stays unchanged, and
    /// the old value is returned correctly.
    #[test]
    fn test_get_mut_and_conflict_update() {
        let mut tree = BPlusTree::<4, _, i32, i32>::new_in(OrdComparer::new()).unwrap();
        for key in 0..3 {
            let _ = tree.insert(key, key * 10);
        }

        if let Some((key, value)) = tree.get_mut(1) {
            assert_eq!(*key, 1);
            *value = 11;
        } else {
            panic!("get_mut 应命中");
        }
        assert_eq!(tree.get(1), Some((&1, &11)));

        let conflict = tree.try_insert(1, 99).unwrap_err();
        assert_eq!(conflict.conflict(), Some(&(1, 99)));
        {
            let ((key, value), pair) = conflict.into_parts();
            assert_eq!(*key, 1);
            assert_eq!(pair, Some((1, 99)));
            *value = 12;
        }
        assert_eq!(tree.get(1), Some((&1, &12)));

        assert_eq!(tree.insert(1, 13), Some(12));
        assert_eq!(tree.get(1), Some((&1, &13)));
        assert_eq!(tree.len(), 3);
        tree.check_invariants_();
    }

    /// 测试目标：验证 `try_insert_by` 只在确实插入时调用 factory。
    /// Test goal: Verify that `try_insert_by` calls the factory only when an
    /// insertion actually happens.
    ///
    /// 测试手段：用 `Cell` 统计 factory 调用次数，先插入重复 key，再插入新 key。
    /// Test method: Count factory calls with a `Cell`, first inserting a duplicate
    /// key and then a new key.
    ///
    /// 判定标准：重复插入时调用次数不增加，新 key 插入时恰好增加一次；冲突信息
    /// 的 `conflict` 为 `None`。
    /// Assertions: The call count does not increase for a duplicate, increases by
    /// exactly one for a new key, and conflict information has `conflict == None`.
    #[test]
    fn test_factory_only_on_success() {
        let mut tree = BPlusTree::<4, _, i32, i32>::new_in(OrdComparer::new()).unwrap();
        let cmp = OrdComparer::new();
        let calls = std::cell::Cell::new(0usize);

        let result = tree.try_insert_by(&1, &cmp, |_| {
            calls.set(calls.get() + 1);
            (1, 10)
        });
        assert!(result.is_ok());
        assert_eq!(calls.get(), 1);

        let conflict = tree
            .try_insert_by(&1, &cmp, |_| {
                calls.set(calls.get() + 1);
                (1, 99)
            })
            .unwrap_err();
        assert_eq!(calls.get(), 1);
        assert!(conflict.conflict().is_none());
        assert_eq!(conflict.existing(), (&1, &10));

        let result = tree.try_insert_by(&2, &cmp, |_| {
            calls.set(calls.get() + 1);
            (2, 20)
        });
        assert!(result.is_ok());
        assert_eq!(calls.get(), 2);
        tree.check_invariants_();
    }

    struct StringStrCmp;

    impl TrComparer<String, str> for StringStrCmp {
        fn compare(&self, a: &String, b: &str) -> Ordering {
            a.as_str().cmp(b)
        }
    }

    /// 测试目标：验证 `str` 这类 unsized 查询对象可以直接用于异构查询。
    /// Test goal: Verify that unsized query types such as `str` can be used directly
    /// for heterogeneous lookup.
    ///
    /// 测试手段：树中保存 `String`，使用 `TrComparer<String, str>` 进行插入、
    /// 查询、可变查询与删除。
    /// Test method: Store `String` and use `TrComparer<String, str>` for insertion,
    /// lookup, mutable lookup, and removal.
    ///
    /// 判定标准：`&str` 查询全部命中正确元素，删除返回正确 value。
    /// Assertions: All `&str` queries hit the correct element and removal returns
    /// the expected value.
    #[test]
    fn test_unsized_str_query() {
        let mut tree = BPlusTree::<4, _, String, i32>::new_in(OrdComparer::new()).unwrap();
        let cmp = StringStrCmp;

        for text in ["alpha", "beta", "gamma"] {
            let result =
                tree.try_insert_by(text, &cmp, |text| (text.to_string(), text.len() as i32));
            assert!(result.is_ok());
        }

        assert_eq!(
            tree.get_by("beta", &cmp)
                .map(|(key, value)| (key.as_str(), *value)),
            Some(("beta", 4))
        );

        if let Some((key, value)) = tree.get_mut_by("beta", &cmp) {
            assert_eq!(key.as_str(), "beta");
            *value = 40;
        } else {
            panic!("get_mut_by 应命中");
        }
        assert_eq!(tree.get_by("beta", &cmp).map(|(_, value)| *value), Some(40));

        assert_eq!(tree.remove_by("alpha", &cmp), Some(5));
        assert!(tree.get_by("alpha", &cmp).is_none());
        tree.check_invariants_();
    }

    /// 测试目标：验证 `remove` 返回 value、`remove_entry` 返回完整键值对。
    /// Test goal: Verify that `remove` returns the value and `remove_entry` returns
    /// the full key-value pair.
    ///
    /// 测试手段：插入两个元素，分别调用 `remove` 与 `remove_entry`。
    /// Test method: Insert two elements and call `remove` and `remove_entry`
    /// respectively.
    ///
    /// 判定标准：`remove` 返回旧 value，`remove_entry` 返回 `(K, V)`，之后两个 key
    /// 都不可查询。
    /// Assertions: `remove` returns the old value, `remove_entry` returns `(K, V)`,
    /// and both keys become unreachable afterwards.
    #[test]
    fn test_remove_value_and_entry() {
        let mut tree = BPlusTree::<4, _, i32, i32>::new_in(OrdComparer::new()).unwrap();
        let _ = tree.insert(1, 10);
        let _ = tree.insert(2, 20);

        assert_eq!(tree.remove(1), Some(10));
        assert_eq!(tree.remove_entry(2), Some((2, 20)));
        assert_eq!(tree.len(), 0);
        assert!(tree.is_empty());
        tree.check_invariants_();
    }

    #[derive(Debug)]
    struct DropKey {
        id: u64,
        counter: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Drop for DropKey {
        fn drop(&mut self) {
            self.counter.set(self.counter.get() + 1);
        }
    }

    struct DropValue {
        counter: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Drop for DropValue {
        fn drop(&mut self) {
            self.counter.set(self.counter.get() + 1);
        }
    }

    struct DropKeyCmp;

    impl TrComparer<DropKey> for DropKeyCmp {
        fn compare(&self, a: &DropKey, b: &DropKey) -> Ordering {
            a.id.cmp(&b.id)
        }
    }

    struct DropQueryCmp;

    impl TrComparer<DropKey, u64> for DropQueryCmp {
        fn compare(&self, a: &DropKey, b: &u64) -> Ordering {
            a.id.cmp(b)
        }
    }

    /// 测试目标：验证分裂、合并与删除过程中没有 Key/Value 的重复析构或泄漏。
    /// Test goal: Verify that splits, merges, and removals never double-drop or leak
    /// keys and values.
    ///
    /// 测试手段：让 key 与 value 都带析构计数器，插入 128 个元素，按 u64 查询
    /// 删除其中 43 个并立即析构返回的 pair，最后 drop 整棵树。
    /// Test method: Give both key and value drop counters, insert 128 entries, remove
    /// 43 of them by a `u64` query and drop the returned pairs immediately, then drop
    /// the whole tree.
    ///
    /// 判定标准：删除 43 个后计数为 86；整棵树 drop 后计数恰好为 256，说明每个
    /// 对象只析构一次，且空闲槽位没有被重复析构。
    /// Assertions: The counter is 86 after removing 43 entries and exactly 256 after
    /// dropping the tree, proving every object is dropped once and freed slots are
    /// not dropped again.
    #[test]
    fn test_drop_counts() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let query_cmp = DropQueryCmp;

        {
            let mut tree = BPlusTree::<4, _, DropKey, DropValue>::new_in(DropKeyCmp).unwrap();

            for id in 0..128u64 {
                let result = tree.try_insert(
                    DropKey {
                        id,
                        counter: std::rc::Rc::clone(&counter),
                    },
                    DropValue {
                        counter: std::rc::Rc::clone(&counter),
                    },
                );
                assert!(result.is_ok());
            }

            for id in (0..128u64).step_by(3) {
                let pair = tree
                    .remove_entry_by(&id, &query_cmp)
                    .expect("按 id 应能删除");
                drop(pair);
            }
            assert_eq!(counter.get(), 86);
        }

        assert_eq!(counter.get(), 256);
    }
}
