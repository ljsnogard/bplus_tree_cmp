//! 基于 Comparer 定序的 B+ Tree。
//!
//! 树中的节点只保存 `SlotPtr`，不复制 `K`。排序完全由 [`TrComparer`]
//! 定义；查询对象可以与 `K` 不同。
//!
//! # 结构
//!
//! - 数据存放在 `data_arena_` 的 `SlabList<(K, V), ...>` 中；
//! - 节点存放在 `node_arena_` 的 `SlabList<TreeNode, ...>` 中；
//! - 两个 arena 使用公开泛型参数 `Cfg: TrConfig` 指定的 Chunk 大小与 allocator；
//!   默认是 `DefaultSlabConfig`，即 64 KiB + `Global`；
//! - 索引项为 `(子树最大 key 的 SlotPtr, 孩子指针)`；
//! - 叶子通过 `prev` / `next` 组成有序链，树自己持有首尾叶指针；
//! - 每个节点保存 `parent_` 父指针，插入/删除沿父链向上传播。
//!
//! # Comparer 一致性
//!
//! 树中元素的顺序由构造时传入的 `C` 固定。`get_with_comparer` / `remove_entry_with_comparer` /
//! `try_insert_with_comparer` 等接受额外 Comparer 的方法，其 Comparer 必须在 `K ↔ K`
//! 上与 `C` 一致，否则查找和插入位置没有意义。
//!
//! # 示例
//!
//! ```
//! use bplustree_cmp::BPlusTree;
//!
//! let mut tree = BPlusTree::<4, _, u64, &str>::new().unwrap();
//! tree.insert(2, "two");
//! tree.insert(1, "one");
//! tree.insert(3, "three");
//!
//! let pairs: std::vec::Vec<_> = tree.iter().map(|(k, v)| (*k, *v)).collect();
//! assert_eq!(pairs, std::vec![(1, "one"), (2, "two"), (3, "three")]);
//! ```

use alloc::alloc::Global;
use core::{
    borrow::Borrow,
    cmp::Ordering,
    iter::FusedIterator,
    marker::PhantomData,
    mem,
    ops::{Bound, RangeBounds},
};

use crate::{
    comparer_::{OrdComparer, QuerySlotComparer, SlotPairKeyComparer, TrComparer},
    ordered_arr_::OrderedArray,
    slab_list_::{DefaultSlabConfig, SlabList, SlotPtr, TrConfig},
    tree_error_::TreeError,
};

use super::node_::{
    DataLeaf, IndexNode, IndexPtrOrdArr, NodeBase, TreeNode, TreeNodePtr, child_at_, child_index_,
    node_is_leaf_, node_len_, node_max_, node_mut_, node_parent_, node_ref_, node_set_parent_,
    reparent_children_,
};

use super::search_::{find_in_leaf_, find_leaf_};

/// 数据位置：叶子句柄与叶子内逻辑下标。
type DataPosition<const M: usize, K, V> = (TreeNodePtr<M, K, V>, usize);

/// 一个范围的首、尾数据位置。
type RangePositions<const M: usize, K, V> = (DataPosition<M, K, V>, DataPosition<M, K, V>);

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

/// 计算范围迭代的起始位置 `(叶子, 叶子内下标)`。
///
/// `Unbounded` 直接使用首叶；有界起点先按 separators 下沉到候选叶子，再用
/// 二分查找定位第一个满足边界的位置。若位置越过叶子末尾，则跳到下一个叶子。
fn range_start_<const M: usize, K, V, Q, TyCmp>(
    root: Option<TreeNodePtr<M, K, V>>,
    first_leaf: Option<TreeNodePtr<M, K, V>>,
    start: Bound<&Q>,
    cmp: &TyCmp,
) -> Option<DataPosition<M, K, V>>
where
    Q: ?Sized,
    TyCmp: TrComparer<K, Q>,
{
    let (query, inclusive) = match start {
        Bound::Unbounded => return first_leaf.map(|leaf| (leaf, 0)),
        Bound::Included(query) => (query, true),
        Bound::Excluded(query) => (query, false),
    };

    let root = root?;
    let query_cmp = QuerySlotComparer::new_(cmp);
    let leaf_ptr = find_leaf_(root, query, &query_cmp);

    // SAFETY: leaf_ptr 由树维护；调用期间树处于只读状态。
    let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
    let pos = if inclusive {
        leaf.order_arr_.lower_bound_by(query, &query_cmp)
    } else {
        leaf.order_arr_.upper_bound_by(query, &query_cmp)
    };

    if pos < leaf.len_() {
        Option::Some((leaf_ptr, pos))
    } else {
        leaf.next_sibl_.map(|next| (next, 0))
    }
}

/// 计算范围迭代的结束位置 `(叶子, 叶子内下标)`，即范围内最后一个元素。
///
/// `Unbounded` 使用尾叶的最后一个元素；有界终点先下沉到候选叶子，再按边界
/// 语义向前回退。结果为空时返回 `None`。
fn range_end_<const M: usize, K, V, Q, TyCmp>(
    root: Option<TreeNodePtr<M, K, V>>,
    last_leaf: Option<TreeNodePtr<M, K, V>>,
    end: Bound<&Q>,
    cmp: &TyCmp,
) -> Option<DataPosition<M, K, V>>
where
    Q: ?Sized,
    TyCmp: TrComparer<K, Q>,
{
    let (query, inclusive) = match end {
        Bound::Unbounded => {
            let leaf_ptr = last_leaf?;
            // SAFETY: last_leaf 由树维护；调用期间树处于只读状态。
            let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
            let last_idx = leaf.len_().checked_sub(1)?;
            return Option::Some((leaf_ptr, last_idx));
        }
        Bound::Included(query) => (query, true),
        Bound::Excluded(query) => (query, false),
    };

    let root = root?;
    let query_cmp = QuerySlotComparer::new_(cmp);
    let leaf_ptr = find_leaf_(root, query, &query_cmp);

    // SAFETY: leaf_ptr 由树维护；调用期间树处于只读状态。
    let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
    let pos = if inclusive {
        leaf.order_arr_.upper_bound_by(query, &query_cmp)
    } else {
        leaf.order_arr_.lower_bound_by(query, &query_cmp)
    };

    if pos > 0 {
        return Option::Some((leaf_ptr, pos - 1));
    }

    let prev_ptr = leaf.prev_sibl_?;
    // SAFETY: prev_sibl_ 由树维护，指向仍然存活的叶子。
    let prev = unsafe { node_ref_(prev_ptr) }.as_leaf_()?;
    prev.len_().checked_sub(1).map(|idx| (prev_ptr, idx))
}

/// 判断 `key` 是否已经越过范围的上界。
fn past_end_<K, Q, TyCmp>(key: &K, end: Bound<&Q>, cmp: &TyCmp) -> bool
where
    Q: ?Sized,
    TyCmp: TrComparer<K, Q>,
{
    match end {
        Bound::Unbounded => false,
        Bound::Included(end) => cmp.compare(key, end) == Ordering::Greater,
        Bound::Excluded(end) => cmp.compare(key, end) != Ordering::Less,
    }
}

/// 判断 `key` 是否位于范围下界之前。
fn before_start_<K, Q, TyCmp>(key: &K, start: Bound<&Q>, cmp: &TyCmp) -> bool
where
    Q: ?Sized,
    TyCmp: TrComparer<K, Q>,
{
    match start {
        Bound::Unbounded => false,
        Bound::Included(start) => cmp.compare(key, start) == Ordering::Less,
        Bound::Excluded(start) => cmp.compare(key, start) != Ordering::Greater,
    }
}

/// 取得某个数据位置上的 key 引用。
///
/// # 安全性
///
/// `(ptr, idx)` 必须指向树中仍然存活的数据项，且返回引用的生命周期内该项
/// 不能被删除或移动。
unsafe fn range_key_at_<'a, const M: usize, K: 'a, V: 'a>(
    ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> Option<&'a K> {
    // SAFETY: 调用方保证位置有效。
    let leaf = unsafe { node_ref_(ptr) }.as_leaf_()?;
    let (data_ptr, ()) = leaf.order_arr_.get_ref_at(idx).ok()?;
    // SAFETY: data_ptr 指向仍然存活的数据项。
    Option::Some(unsafe { data_pair_(*data_ptr) }.0)
}

/// 计算一个范围在树中的首、尾位置。
///
/// 先分别定位第一个不低于下界的元素和最后一个不高于上界的元素，再用这两个
/// 候选元素检查它们是否真的落在范围内。树中没有任何键落在区间内时返回
/// `None`，因此调用方不需要在迭代过程中重复比较边界。
fn range_positions_<const M: usize, K, V, Q, TyCmp, R>(
    root: Option<TreeNodePtr<M, K, V>>,
    first_leaf: Option<TreeNodePtr<M, K, V>>,
    last_leaf: Option<TreeNodePtr<M, K, V>>,
    range: &R,
    cmp: &TyCmp,
) -> Option<RangePositions<M, K, V>>
where
    Q: ?Sized,
    TyCmp: TrComparer<K, Q>,
    R: RangeBounds<Q>,
{
    let start = range.start_bound();
    let end = range.end_bound();
    let front = range_start_(root, first_leaf, start, cmp)?;
    let back = range_end_(root, last_leaf, end, cmp)?;

    // SAFETY: front/back 来自当前存活树的搜索结果。
    let front_key = unsafe { range_key_at_(front.0, front.1) }?;
    // SAFETY: 同上。
    let back_key = unsafe { range_key_at_(back.0, back.1) }?;

    if past_end_(front_key, end, cmp) || before_start_(back_key, start, cmp) {
        Option::None
    } else {
        Option::Some((front, back))
    }
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

/// 从 `current` 开始沿父链向上刷新 separator。
///
/// `child_max` 是 `current` 子树的当前最大 key。每层把父节点中对应
/// `(separator, child)` 的 separator 更新为 `child_max`；当父节点最大 key
/// 没有变化时停止。
fn refresh_separators_<const M: usize, K, V, C>(
    mut current: TreeNodePtr<M, K, V>,
    mut child_max: SlotPtr<(K, V)>,
    cmp: &C,
) where
    C: TrComparer<SlotPtr<(K, V)>>,
{
    loop {
        let Option::Some(parent_ptr) = node_parent_(current) else {
            return;
        };
        let Option::Some(idx) = child_index_(parent_ptr, current) else {
            return;
        };
        let old_parent_max = node_max_(parent_ptr);

        {
            // SAFETY: parent_ptr 来自当前树的父指针；此处没有其他别名。
            let parent = match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                Option::Some(parent) => parent,
                Option::None => return,
            };
            set_separator_at_(&mut parent.children_, idx, child_max, cmp);
        }

        let new_parent_max = node_max_(parent_ptr);
        if new_parent_max == old_parent_max {
            return;
        }
        let Option::Some(new_max) = new_parent_max else {
            return;
        };
        child_max = new_max;
        current = parent_ptr;
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
fn split_leaf_with_<const M: usize, K, V, C, Cfg>(
    nodes: &mut SlabList<TreeNode<M, K, V>, Cfg>,
    leaf_ptr: TreeNodePtr<M, K, V>,
    pending: SlotPtr<(K, V)>,
    cmp: &C,
    allocator: &Cfg::Allocator,
) -> SplitLeafResult<M, K, V>
where
    C: TrComparer<SlotPtr<(K, V)>>,
    Cfg: TrConfig,
{
    let old_next = match unsafe { node_ref_(leaf_ptr) }.as_leaf_() {
        Option::Some(leaf) => leaf.next_sibl_,
        Option::None => unreachable!("split_leaf_with_ 只接受叶子节点"),
    };
    let old_parent = node_parent_(leaf_ptr);

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

    let right_ptr = nodes.insert(
        TreeNode::Leaf(DataLeaf {
            base_: NodeBase::new_(),
            order_arr_: right_arr,
            prev_sibl_: Option::Some(leaf_ptr),
            next_sibl_: old_next,
        }),
        allocator,
    );
    node_set_parent_(right_ptr, old_parent);

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
fn split_index_with_<const M: usize, K, V, C, Cfg>(
    nodes: &mut SlabList<TreeNode<M, K, V>, Cfg>,
    index_ptr: TreeNodePtr<M, K, V>,
    pending: (SlotPtr<(K, V)>, TreeNodePtr<M, K, V>),
    cmp: &C,
    allocator: &Cfg::Allocator,
) -> SplitIndexResult<M, K, V>
where
    C: TrComparer<SlotPtr<(K, V)>>,
    Cfg: TrConfig,
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

    let old_parent = node_parent_(index_ptr);
    let right_ptr = nodes.insert(
        TreeNode::Index(IndexNode {
            base_: NodeBase::new_(),
            children_: right_arr,
        }),
        allocator,
    );
    node_set_parent_(right_ptr, old_parent);
    reparent_children_(right_ptr);

    (left_max, right_max, right_ptr)
}

/// 用两个 `(separator, child)` 项创建新的索引 root。
#[allow(clippy::too_many_arguments)]
fn create_root_with_two_<const M: usize, K, V, C, Cfg>(
    nodes: &mut SlabList<TreeNode<M, K, V>, Cfg>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    left_ptr: TreeNodePtr<M, K, V>,
    left_max: SlotPtr<(K, V)>,
    right_ptr: TreeNodePtr<M, K, V>,
    right_max: SlotPtr<(K, V)>,
    cmp: &C,
    allocator: &Cfg::Allocator,
) where
    C: TrComparer<SlotPtr<(K, V)>>,
    Cfg: TrConfig,
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

    let root_ptr = nodes.insert(TreeNode::Index(index), allocator);
    node_set_parent_(left_ptr, Option::Some(root_ptr));
    node_set_parent_(right_ptr, Option::Some(root_ptr));
    *root = Option::Some(root_ptr);
}

/// 把新数据句柄插入树中；数据本身已经写入 data arena。
///
/// 该函数负责叶子插入、分裂、沿父指针向上传播以及根分裂。
fn insert_data_into_tree_<const M: usize, K, V, C, Cfg>(
    nodes: &mut SlabList<TreeNode<M, K, V>, Cfg>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    last_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    leaf_ptr: TreeNodePtr<M, K, V>,
    data_ptr: SlotPtr<(K, V)>,
    cmp: &C,
    allocator: &Cfg::Allocator,
) where
    C: TrComparer<SlotPtr<(K, V)>>,
    Cfg: TrConfig,
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
        refresh_separators_(leaf_ptr, leaf_max, cmp);
        return;
    }

    let (_left_max, right_max, right_ptr, old_next) =
        split_leaf_with_(nodes, leaf_ptr, data_ptr, cmp, allocator);
    if old_next.is_none() {
        *last_leaf = Option::Some(right_ptr);
    }

    let mut current = leaf_ptr;
    let mut pending = Option::Some((right_max, right_ptr));

    loop {
        let Option::Some(parent_ptr) = node_parent_(current) else {
            // `current` 是 root：如果还有上移项，创建新的内部 root。
            if let Option::Some((right_sep, right_child)) = pending {
                let Option::Some(left_max) = node_max_(current) else {
                    return;
                };
                create_root_with_two_(
                    nodes,
                    root,
                    current,
                    left_max,
                    right_child,
                    right_sep,
                    cmp,
                    allocator,
                );
            }
            return;
        };

        let Option::Some(child_idx) = child_index_(parent_ptr, current) else {
            return;
        };
        let parent_len = node_len_(parent_ptr);
        let old_parent_max = node_max_(parent_ptr);
        let Option::Some(current_max) = node_max_(current) else {
            return;
        };

        {
            // SAFETY: parent_ptr 来自当前树的父指针；此处没有其他别名。
            let parent = match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                Option::Some(parent) => parent,
                Option::None => return,
            };
            set_separator_at_(&mut parent.children_, child_idx, current_max, cmp);
        }

        match pending.take() {
            Option::Some((right_sep, right_child)) => {
                if parent_len < M {
                    {
                        // SAFETY: parent_ptr 来自当前树的父指针；此处没有其他别名。
                        let parent = match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                            Option::Some(parent) => parent,
                            Option::None => return,
                        };
                        let result = parent
                            .children_
                            .try_insert_pair_by((right_sep, right_child), cmp);
                        debug_assert!(result.is_succ());
                    }

                    let new_parent_max = node_max_(parent_ptr);
                    if new_parent_max == old_parent_max {
                        return;
                    }
                    current = parent_ptr;
                } else {
                    let (_left, right, right_node) = split_index_with_(
                        nodes,
                        parent_ptr,
                        (right_sep, right_child),
                        cmp,
                        allocator,
                    );
                    pending = Option::Some((right, right_node));
                    current = parent_ptr;
                }
            }
            Option::None => {
                let new_parent_max = node_max_(parent_ptr);
                if new_parent_max == old_parent_max {
                    return;
                }
                current = parent_ptr;
            }
        }
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
        node_set_parent_(pair.1, Option::Some(to_ptr));

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
fn collapse_root_to_child_<const M: usize, K, V, Cfg>(
    nodes: &mut SlabList<TreeNode<M, K, V>, Cfg>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    first_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    last_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    root_ptr: TreeNodePtr<M, K, V>,
    child_ptr: TreeNodePtr<M, K, V>,
    allocator: &Cfg::Allocator,
) where
    Cfg: TrConfig,
{
    let mut old_root = root_ptr;
    let mut child = child_ptr;

    loop {
        *root = Option::Some(child);
        node_set_parent_(child, Option::None);
        let _ = nodes.remove(old_root, allocator);

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
fn clear_root_<const M: usize, K, V, Cfg>(
    nodes: &mut SlabList<TreeNode<M, K, V>, Cfg>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    first_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    last_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    root_ptr: TreeNodePtr<M, K, V>,
    allocator: &Cfg::Allocator,
) where
    Cfg: TrConfig,
{
    let _ = nodes.remove(root_ptr, allocator);
    *root = Option::None;
    *first_leaf = Option::None;
    *last_leaf = Option::None;
}

/// 在删除一项后修复节点占用与 separator。
///
/// 从 `current_ptr` 开始沿父指针向上修复。`current_max` 是当前节点子树的
/// 最大 key；当前节点已经是 root 时，本函数也负责根收缩。
#[allow(clippy::collapsible_if)] // 保留“先左后右”的借位语义，避免 let-chain 降低可读性
#[allow(clippy::too_many_arguments)] // 根、首尾叶、当前节点、sep、comparer 与 allocator 都是必要上下文
fn rebalance_after_remove_<const M: usize, K, V, C, Cfg>(
    nodes: &mut SlabList<TreeNode<M, K, V>, Cfg>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    first_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    last_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    mut current_ptr: TreeNodePtr<M, K, V>,
    mut current_max: Option<SlotPtr<(K, V)>>,
    cmp: &C,
    allocator: &Cfg::Allocator,
) where
    C: TrComparer<SlotPtr<(K, V)>>,
    Cfg: TrConfig,
{
    let min = min_occupancy_(M);

    loop {
        // 1. 当前节点已经是 root。
        let Option::Some(parent_ptr) = node_parent_(current_ptr) else {
            let (is_leaf, node_len) = {
                let node = unsafe { node_ref_(current_ptr) };
                (node.is_leaf_(), node.len_())
            };

            if is_leaf {
                if node_len == 0 {
                    unlink_leaf_(current_ptr, first_leaf, last_leaf);
                    clear_root_(nodes, root, first_leaf, last_leaf, current_ptr, allocator);
                }
                return;
            }

            match node_len {
                0 => clear_root_(nodes, root, first_leaf, last_leaf, current_ptr, allocator),
                1 => {
                    let Option::Some(child_ptr) = child_at_(current_ptr, 0) else {
                        return;
                    };
                    collapse_root_to_child_(
                        nodes,
                        root,
                        first_leaf,
                        last_leaf,
                        current_ptr,
                        child_ptr,
                        allocator,
                    );
                }
                _ => {}
            }
            return;
        };

        let Option::Some(idx) = child_index_(parent_ptr, current_ptr) else {
            return;
        };
        let parent_len = node_len_(parent_ptr);

        // 2. root 只有一个孩子：root 收缩为该孩子。
        if node_parent_(parent_ptr).is_none() && parent_len == 1 {
            if node_len_(current_ptr) == 0 {
                if node_is_leaf_(current_ptr) {
                    unlink_leaf_(current_ptr, first_leaf, last_leaf);
                }
                let _ = nodes.remove(current_ptr, allocator);
                clear_root_(nodes, root, first_leaf, last_leaf, parent_ptr, allocator);
            } else {
                collapse_root_to_child_(
                    nodes,
                    root,
                    first_leaf,
                    last_leaf,
                    parent_ptr,
                    current_ptr,
                    allocator,
                );
            }
            return;
        }

        // 3. 当前节点不再低于最小占用：刷新 separator 后结束。
        if node_len_(current_ptr) >= min {
            if let Option::Some(max) = current_max {
                refresh_separators_(current_ptr, max, cmp);
            }
            return;
        }

        // 4. 父节点只有一个孩子：折叠这一层冗余的索引。
        if parent_len == 1 {
            if node_len_(current_ptr) == 0 {
                if node_is_leaf_(current_ptr) {
                    unlink_leaf_(current_ptr, first_leaf, last_leaf);
                }
                let _ = nodes.remove(current_ptr, allocator);

                match unsafe { node_mut_(parent_ptr) }.as_index_mut_() {
                    Option::Some(parent) => {
                        let _ = parent.children_.try_remove_at(0);
                    }
                    Option::None => return,
                }

                current_ptr = parent_ptr;
                current_max = Option::None;
                continue;
            }

            // 父节点只有当前孩子，直接用当前节点替换父节点。
            let Option::Some(grand_ptr) = node_parent_(parent_ptr) else {
                return;
            };
            let Option::Some(grand_idx) = child_index_(grand_ptr, parent_ptr) else {
                return;
            };
            match unsafe { node_mut_(grand_ptr) }.as_index_mut_() {
                Option::Some(grand) => match grand.children_.get_mut_at(grand_idx) {
                    Ok((_, child_slot)) => *child_slot = current_ptr,
                    Err(_) => return,
                },
                Option::None => return,
            }
            node_set_parent_(current_ptr, Option::Some(grand_ptr));
            let _ = nodes.remove(parent_ptr, allocator);
            current_max = node_max_(current_ptr);
            continue;
        }

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
                                if let Option::Some(parent_max) = node_max_(parent_ptr) {
                                    refresh_separators_(parent_ptr, parent_max, cmp);
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
                                if let Option::Some(parent_max) = node_max_(parent_ptr) {
                                    refresh_separators_(parent_ptr, parent_max, cmp);
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
            let _ = nodes.remove(victim, allocator);

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
                            node_set_parent_(pair.1, Option::Some(current_ptr));
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
                                if let Option::Some(parent_max) = node_max_(parent_ptr) {
                                    refresh_separators_(parent_ptr, parent_max, cmp);
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
                            node_set_parent_(pair.1, Option::Some(current_ptr));
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
                                if let Option::Some(parent_max) = node_max_(parent_ptr) {
                                    refresh_separators_(parent_ptr, parent_max, cmp);
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
            let _ = nodes.remove(victim, allocator);

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
        }
    }
}

/// 插入查找结果。
enum InsertLocate<const M: usize, K, V> {
    /// 树为空。
    Empty,
    /// 已存在等价 key。
    Conflict(SlotPtr<(K, V)>),
    /// 不存在等价 key，给出目标叶子。
    Missing(TreeNodePtr<M, K, V>),
}

/// 查找插入位置。
fn locate_for_insert_parts_<const M: usize, K, V, Q, TyCmp>(
    root: Option<TreeNodePtr<M, K, V>>,
    query: &Q,
    cmp: &TyCmp,
) -> InsertLocate<M, K, V>
where
    Q: ?Sized,
    TyCmp: TrComparer<K, Q>,
{
    let Option::Some(root) = root else {
        return InsertLocate::Empty;
    };

    let query_cmp = QuerySlotComparer::new_(cmp);
    let leaf_ptr = find_leaf_(root, query, &query_cmp);

    // SAFETY: 叶子指针由树维护；搜索期间结构不变。
    let node = unsafe { node_ref_(leaf_ptr) };
    let Option::Some(leaf) = node.as_leaf_() else {
        return InsertLocate::Missing(leaf_ptr);
    };

    match find_in_leaf_(leaf, query, &query_cmp) {
        Ok((_, data_ptr)) => InsertLocate::Conflict(data_ptr),
        Err(_) => InsertLocate::Missing(leaf_ptr),
    }
}

impl<const M: usize, C, K, V, Cfg> BPlusTree<M, C, K, V, Cfg>
where
    C: TrComparer<K>,
    Cfg: TrConfig,
{
    /// 返回节点容量 `M`。
    pub const fn degree(&self) -> usize {
        M
    }

    /// 使用给定 comparer 和 allocator 创建空树。
    ///
    /// 两个 arena 使用同一个 allocator 实例；`SlabList` 在分配/释放 Chunk
    /// 时按需借用它，因此 allocator 不需要实现 `Clone`。
    fn new_with_allocator_(comparer: C, allocator: Cfg::Allocator) -> Result<Self, TreeError> {
        assert!(M >= 2, "B+ 树的节点容量 M 必须至少为 2");
        Ok(Self {
            len_: 0,
            comparer_: comparer,
            tree_root_: Option::None,
            first_leaf_: Option::None,
            last_leaf_: Option::None,
            allocator_: allocator,
            data_arena_: SlabList::new().map_err(TreeError::from_slab_)?,
            node_arena_: SlabList::new().map_err(TreeError::from_slab_)?,
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
    /// 用完全不同的查询类型，请使用 [`Self::get_with_comparer`]。
    pub fn get<Q>(&self, query: Q) -> Option<(&K, &V)>
    where
        Q: Borrow<K>,
    {
        let query_ref = query.borrow();
        self.get_with_comparer(query_ref, &self.comparer_)
    }

    /// 使用存储的 comparer 查询并返回可变 value 引用。
    pub fn get_mut<Q>(&mut self, query: Q) -> Option<(&K, &mut V)>
    where
        Q: Borrow<K>,
    {
        let query_ref = query.borrow();
        let root = self.tree_root_?;
        let query_cmp = QuerySlotComparer::new_(&self.comparer_);
        let leaf_ptr = find_leaf_(root, query_ref, &query_cmp);

        // SAFETY: 叶子指针由树维护；查询期间 &mut self 保证没有其他引用。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        let (_, data_ptr) = find_in_leaf_(leaf, query_ref, &query_cmp).ok()?;

        // SAFETY: data_ptr 指向存活数据；&mut self 保证独占访问。
        Option::Some(unsafe { data_pair_mut_(data_ptr) })
    }

    /// 使用异构 comparer 查询与 `query` 等价的元素。
    ///
    /// `cmp` 必须在 `K ↔ K` 上与树的存储 Comparer 一致。
    pub fn get_with_comparer<'f, Q, TyCmp>(
        &'f self,
        query: &Q,
        cmp: &TyCmp,
    ) -> Option<(&'f K, &'f V)>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        let root = self.tree_root_?;
        let query_cmp = QuerySlotComparer::new_(cmp);
        let leaf_ptr = find_leaf_(root, query, &query_cmp);

        // SAFETY: 叶子指针由树维护；&self 保证期间结构不变。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        let (_, data_ptr) = find_in_leaf_(leaf, query, &query_cmp).ok()?;

        // SAFETY: data_ptr 指向存活数据；&self 的返回生命周期内不会被释放。
        Option::Some(unsafe { data_pair_(data_ptr) })
    }

    /// 使用异构 comparer 查询并返回可变 value 引用。
    pub fn get_mut_with_comparer<'f, Q, TyCmp>(
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
        let leaf_ptr = find_leaf_(root, query, &query_cmp);

        // SAFETY: 叶子指针由树维护；&mut self 保证没有其他引用。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        let (_, data_ptr) = find_in_leaf_(leaf, query, &query_cmp).ok()?;

        // SAFETY: data_ptr 指向存活数据；&mut self 保证独占访问。
        Option::Some(unsafe { data_pair_mut_(data_ptr) })
    }

    /// 在空树中插入第一个元素。
    fn insert_first_entry_(&mut self, pair: (K, V)) -> SlotPtr<(K, V)> {
        let data_ptr = self.data_arena_.insert(pair, &self.allocator_);
        let node_cmp = SlotPairKeyComparer::new_(&self.comparer_);

        let mut leaf = DataLeaf::new_();
        let result = leaf
            .order_arr_
            .try_insert_pair_by((data_ptr, ()), &node_cmp);
        debug_assert!(result.is_succ());

        let leaf_ptr = self
            .node_arena_
            .insert(TreeNode::Leaf(leaf), &self.allocator_);
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
    ) {
        let node_cmp = SlotPairKeyComparer::new_(&self.comparer_);
        insert_data_into_tree_(
            &mut self.node_arena_,
            &mut self.tree_root_,
            &mut self.last_leaf_,
            leaf_ptr,
            data_ptr,
            &node_cmp,
            &self.allocator_,
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
    pub fn try_insert_with_comparer<'f, Q, TyCmp>(
        &'f mut self,
        query: &Q,
        cmp: &TyCmp,
        factory: impl FnOnce(&Q) -> (K, V),
    ) -> Result<(&'f K, &'f mut V), ConflictInfo<'f, K, V>>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        let located = locate_for_insert_parts_(self.tree_root_, query, cmp);

        match located {
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
            InsertLocate::Missing(leaf_ptr) => {
                let data_ptr = self.data_arena_.insert(factory(query), &self.allocator_);
                self.insert_data_after_locate_(data_ptr, leaf_ptr);
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

        let located = locate_for_insert_parts_(self.tree_root_, &pair.0, &self.comparer_);

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
            InsertLocate::Missing(leaf_ptr) => {
                let data_ptr = self.data_arena_.insert(pair, &self.allocator_);
                self.insert_data_after_locate_(data_ptr, leaf_ptr);
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
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, &str>::new().unwrap();
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
        remove_entry_with_comparer_(
            &mut self.data_arena_,
            &mut self.node_arena_,
            &mut self.tree_root_,
            &mut self.first_leaf_,
            &mut self.last_leaf_,
            &mut self.len_,
            query_ref,
            &self.comparer_,
            &self.comparer_,
            &self.allocator_,
        )
    }

    /// 使用异构 comparer 删除与 `query` 等价的元素，返回 value。
    pub fn remove_with_comparer<Q, TyCmp>(&mut self, query: &Q, cmp: &TyCmp) -> Option<V>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        self.remove_entry_with_comparer(query, cmp)
            .map(|(_, value)| value)
    }

    /// 使用异构 comparer 删除与 `query` 等价的元素，返回完整 `(K, V)`。
    ///
    /// `cmp` 必须在 `K ↔ K` 上与树的存储 Comparer 一致。
    pub fn remove_entry_with_comparer<Q, TyCmp>(&mut self, query: &Q, cmp: &TyCmp) -> Option<(K, V)>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        remove_entry_with_comparer_(
            &mut self.data_arena_,
            &mut self.node_arena_,
            &mut self.tree_root_,
            &mut self.first_leaf_,
            &mut self.last_leaf_,
            &mut self.len_,
            query,
            cmp,
            &self.comparer_,
            &self.allocator_,
        )
    }

    /// 返回按 key 升序迭代的迭代器。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
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

    /// 返回按 key 升序迭代的可变迭代器。
    ///
    /// 迭代器只允许修改 value；key 与树结构保持不变。与 [`Self::iter`] 一样，
    /// 它沿叶子链前进，不需要工作栈或额外堆分配。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    /// tree.insert(1, 10);
    /// tree.insert(2, 20);
    /// for (_, value) in tree.iter_mut() {
    ///     *value += 1;
    /// }
    /// assert_eq!(tree.get(1), Some((&1, &11)));
    /// ```
    pub fn iter_mut(&mut self) -> IterMut<'_, M, K, V> {
        IterMut {
            next_leaf_: self.first_leaf_,
            index_: 0,
            remaining_: self.len_,
            _mark_: PhantomData,
        }
    }

    /// 返回按 key 升序迭代 key 的迭代器。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    /// tree.insert(1, 10);
    /// tree.insert(2, 20);
    /// let keys: Vec<i32> = tree.keys().copied().collect();
    /// assert_eq!(keys, vec![1, 2]);
    /// ```
    pub fn keys(&self) -> Keys<'_, M, K, V> {
        Keys { iter_: self.iter() }
    }

    /// 返回按 key 升序迭代 value 的迭代器。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    /// tree.insert(1, 10);
    /// tree.insert(2, 20);
    /// let values: Vec<i32> = tree.values().copied().collect();
    /// assert_eq!(values, vec![10, 20]);
    /// ```
    pub fn values(&self) -> Values<'_, M, K, V> {
        Values { iter_: self.iter() }
    }

    /// 返回按 key 升序迭代可变 value 的迭代器。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    /// tree.insert(1, 10);
    /// for value in tree.values_mut() {
    ///     *value += 1;
    /// }
    /// assert_eq!(tree.get(1), Some((&1, &11)));
    /// ```
    pub fn values_mut(&mut self) -> ValuesMut<'_, M, K, V> {
        ValuesMut {
            iter_: self.iter_mut(),
        }
    }

    /// 判断树中是否存在与 `query` 等价的 key。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    /// tree.insert(1, 10);
    /// assert!(tree.contains_key(1));
    /// assert!(!tree.contains_key(2));
    /// ```
    pub fn contains_key<Q>(&self, query: Q) -> bool
    where
        Q: Borrow<K>,
    {
        self.get(query).is_some()
    }

    /// 返回最小 `(K, V)` 引用，语义同 [`Self::first`]。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    /// tree.insert(2, 20);
    /// tree.insert(1, 10);
    /// assert_eq!(tree.first_key_value(), Some((&1, &10)));
    /// ```
    pub fn first_key_value(&self) -> Option<(&K, &V)> {
        self.first()
    }

    /// 返回最大 `(K, V)` 引用，语义同 [`Self::last`]。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    /// tree.insert(2, 20);
    /// tree.insert(1, 10);
    /// assert_eq!(tree.last_key_value(), Some((&2, &20)));
    /// ```
    pub fn last_key_value(&self) -> Option<(&K, &V)> {
        self.last()
    }

    /// 清空树中所有元素，并释放内部 arena 的 Chunk。
    ///
    /// 该操作只归还已有的 Chunk，不会产生新的堆分配。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    /// tree.insert(1, 10);
    /// tree.clear();
    /// assert!(tree.is_empty());
    /// assert_eq!(tree.get(1), None);
    /// ```
    pub fn clear(&mut self) {
        self.node_arena_.clear(&self.allocator_);
        self.data_arena_.clear(&self.allocator_);
        self.len_ = 0;
        self.tree_root_ = Option::None;
        self.first_leaf_ = Option::None;
        self.last_leaf_ = Option::None;
    }

    /// 返回 `range` 范围内的不可变迭代器。
    ///
    /// 范围边界使用树自身的 Comparer 解释。起点与终点都在构造迭代器时定位，
    /// 迭代过程只沿叶子链前进，因此不需要工作栈或额外堆分配。若范围在
    /// Comparer 顺序下为空，或起点晚于终点，则返回空迭代器。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    /// for key in 0..10 {
    ///     tree.insert(key, key * 10);
    /// }
    ///
    /// let pairs: Vec<(i32, i32)> = tree.range(3..6).map(|(k, v)| (*k, *v)).collect();
    /// assert_eq!(pairs, vec![(3, 30), (4, 40), (5, 50)]);
    /// ```
    pub fn range<R>(&self, range: R) -> RangeIter<'_, M, K, V>
    where
        R: RangeBounds<K>,
    {
        match range_positions_(
            self.tree_root_,
            self.first_leaf_,
            self.last_leaf_,
            &range,
            &self.comparer_,
        ) {
            Option::Some((front, back)) => RangeIter {
                front_: Option::Some(front),
                back_: Option::Some(back),
                _mark_: PhantomData,
            },
            Option::None => RangeIter::empty_(),
        }
    }

    /// 返回 `range` 范围内的可变迭代器。
    ///
    /// 与 [`Self::range`] 相同，但迭代项为 `(&K, &mut V)`。边界为空时返回空
    /// 迭代器。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    /// for key in 0..10 {
    ///     tree.insert(key, key * 10);
    /// }
    /// for (_, value) in tree.range_mut(3..6) {
    ///     *value += 1;
    /// }
    /// assert_eq!(tree.get(3), Some((&3, &31)));
    /// assert_eq!(tree.get(6), Some((&6, &60)));
    /// ```
    pub fn range_mut<R>(&mut self, range: R) -> RangeIterMut<'_, M, K, V>
    where
        R: RangeBounds<K>,
    {
        match range_positions_(
            self.tree_root_,
            self.first_leaf_,
            self.last_leaf_,
            &range,
            &self.comparer_,
        ) {
            Option::Some((front, back)) => RangeIterMut {
                front_: Option::Some(front),
                back_: Option::Some(back),
                _mark_: PhantomData,
            },
            Option::None => RangeIterMut::empty_(),
        }
    }

    /// 使用异构 Comparer 返回 `range` 范围内的不可变迭代器。
    ///
    /// `cmp` 必须在 `K ↔ K` 上与树的存储 Comparer 一致。边界类型 `Q` 可以与
    /// `K` 不同，例如树中保存 `String` 时用 `&str` 作为边界。空范围返回空
    /// 迭代器。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let mut tree = BPlusTree::<4, _, String, i32>::new().unwrap();
    /// for (key, value) in [("a", 1), ("b", 2), ("c", 3)] {
    ///     tree.insert(key.to_string(), value);
    /// }
    ///
    /// let keys: Vec<&str> = tree
    ///     .range_with_comparer("a"..="b", &|a: &String, b: &&str| a.as_str().cmp(*b))
    ///     .map(|(key, _)| key.as_str())
    ///     .collect();
    /// assert_eq!(keys, vec!["a", "b"]);
    /// ```
    pub fn range_with_comparer<'a, Q, TyCmp, R>(
        &'a self,
        range: R,
        cmp: &TyCmp,
    ) -> RangeIter<'a, M, K, V>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
        R: RangeBounds<Q>,
    {
        match range_positions_(
            self.tree_root_,
            self.first_leaf_,
            self.last_leaf_,
            &range,
            cmp,
        ) {
            Option::Some((front, back)) => RangeIter {
                front_: Option::Some(front),
                back_: Option::Some(back),
                _mark_: PhantomData,
            },
            Option::None => RangeIter::empty_(),
        }
    }

    /// 使用异构 Comparer 返回 `range` 范围内的可变迭代器。
    ///
    /// `cmp` 必须在 `K ↔ K` 上与树的存储 Comparer 一致。空范围返回空迭代器。
    pub fn range_mut_with_comparer<'a, Q, TyCmp, R>(
        &'a mut self,
        range: R,
        cmp: &TyCmp,
    ) -> RangeIterMut<'a, M, K, V>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
        R: RangeBounds<Q>,
    {
        match range_positions_(
            self.tree_root_,
            self.first_leaf_,
            self.last_leaf_,
            &range,
            cmp,
        ) {
            Option::Some((front, back)) => RangeIterMut {
                front_: Option::Some(front),
                back_: Option::Some(back),
                _mark_: PhantomData,
            },
            Option::None => RangeIterMut::empty_(),
        }
    }
}

impl<const M: usize, K, V, Cfg> BPlusTree<M, OrdComparer<K>, K, V, Cfg>
where
    K: Ord,
    Cfg: TrConfig<Allocator = Global>,
{
    /// 使用 `OrdComparer` 与 [`Global`] allocator 创建一个空树。
    ///
    /// 这是 [`Self::new_in`] 在 `Global` allocator 下的便捷形式；`Cfg::CHUNK_SIZE` 仍由配置类型决定。
    ///
    /// # Errors
    ///
    /// 若当前 `Cfg::CHUNK_SIZE` 无法容纳至少一个 `(K, V)` 或树节点，返回
    /// [`TreeError`]。
    ///
    /// # Panics
    ///
    /// 当 `M < 2` 时 panic：B+ 树至少需要能容纳两个项的节点。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    ///
    /// let tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    /// assert!(tree.is_empty());
    /// ```
    pub fn new() -> Result<Self, TreeError> {
        Self::new_in(Global)
    }
}

impl<const M: usize, K, V, Cfg> BPlusTree<M, OrdComparer<K>, K, V, Cfg>
where
    K: Ord,
    Cfg: TrConfig,
{
    /// 使用 `OrdComparer` 与给定 allocator 创建一个空树。
    ///
    /// allocator 由树自身持有；数据 arena 和节点 arena 共用同一个实例。
    /// `SlabList` 在分配或释放 Chunk 时借用该 allocator，因此不要求
    /// allocator 不需要实现 `Clone`。
    ///
    /// # Errors
    ///
    /// 若当前 `Cfg::CHUNK_SIZE` 无法容纳至少一个 `(K, V)` 或树节点，返回
    /// [`TreeError`]。
    ///
    /// # Panics
    ///
    /// 当 `M < 2` 时 panic：B+ 树至少需要能容纳两个项的节点。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    /// use std::alloc::Global;
    ///
    /// let tree = BPlusTree::<4, _, i32, i32>::new_in(Global).unwrap();
    /// assert!(tree.is_empty());
    /// ```
    pub fn new_in(allocator: Cfg::Allocator) -> Result<Self, TreeError> {
        Self::new_with_allocator_(OrdComparer::new(), allocator)
    }

    /// 使用 `OrdComparer` 与给定 allocator 从已有 `(K, V)` 集合构建树。
    ///
    /// 重复 key 保留最后一次出现的 value。
    ///
    /// # Errors
    ///
    /// 与 [`Self::new_in`] 相同。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BPlusTree;
    /// use std::alloc::Global;
    ///
    /// let tree =
    ///     BPlusTree::<4, _, i32, i32>::from_iter_in(vec![(2, 20), (1, 10)], Global).unwrap();
    /// assert_eq!(tree.len(), 2);
    /// ```
    pub fn from_iter_in(
        iter: impl IntoIterator<Item = (K, V)>,
        allocator: Cfg::Allocator,
    ) -> Result<Self, TreeError> {
        let mut tree = Self::new_in(allocator)?;
        tree.extend(iter);
        Ok(tree)
    }
}

impl<const M: usize, C, K, V, Cfg> BPlusTree<M, C, K, V, Cfg>
where
    C: TrComparer<K>,
    Cfg: TrConfig<Allocator = Global>,
{
    /// 使用给定 comparer 与 [`Global`] allocator 创建一个空树。
    ///
    /// `K` 不需要实现 `Ord`；排序完全由 `comparer` 决定。
    ///
    /// # Errors
    ///
    /// 若当前 `Cfg::CHUNK_SIZE` 无法容纳至少一个 `(K, V)` 或树节点，返回
    /// [`TreeError`]。
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
    /// let tree =
    ///     BPlusTree::<4, _, i32, i32>::with_comparer(OrdComparer::new()).unwrap();
    /// assert!(tree.is_empty());
    /// ```
    pub fn with_comparer(comparer: C) -> Result<Self, TreeError> {
        Self::with_comparer_in(comparer, Global)
    }

    /// 使用给定 comparer 与 [`Global`] allocator 从已有集合构建树。
    ///
    /// 重复 key 保留最后一次出现的 value。
    ///
    /// # Errors
    ///
    /// 与 [`Self::with_comparer`] 相同。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{BPlusTree, OrdComparer};
    ///
    /// let tree = BPlusTree::<4, _, i32, i32>::from_iter_with_comparer(
    ///     OrdComparer::new(),
    ///     vec![(2, 20), (1, 10)],
    /// )
    /// .unwrap();
    /// assert_eq!(tree.len(), 2);
    /// assert_eq!(tree.first(), Some((&1, &10)));
    /// ```
    pub fn from_iter_with_comparer(
        comparer: C,
        iter: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Self, TreeError> {
        let mut tree = Self::with_comparer(comparer)?;
        tree.extend(iter);
        Ok(tree)
    }
}

impl<const M: usize, C, K, V, Cfg> BPlusTree<M, C, K, V, Cfg>
where
    C: TrComparer<K>,
    Cfg: TrConfig,
{
    /// 使用给定 comparer 与 allocator 创建一个空树。
    ///
    /// allocator 由树自身持有；数据 arena 和节点 arena 共用同一个实例。
    /// `SlabList` 在分配或释放 Chunk 时借用该 allocator，因此不要求
    /// allocator 不需要实现 `Clone`。
    ///
    /// # Errors
    ///
    /// 若当前 `Cfg::CHUNK_SIZE` 无法容纳至少一个 `(K, V)` 或树节点，返回
    /// [`TreeError`]。
    ///
    /// # Panics
    ///
    /// 当 `M < 2` 时 panic：B+ 树至少需要能容纳两个项的节点。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{BPlusTree, OrdComparer};
    /// use std::alloc::Global;
    ///
    /// let tree =
    ///     BPlusTree::<4, _, i32, i32>::with_comparer_in(OrdComparer::new(), Global).unwrap();
    /// assert!(tree.is_empty());
    /// ```
    pub fn with_comparer_in(comparer: C, allocator: Cfg::Allocator) -> Result<Self, TreeError> {
        Self::new_with_allocator_(comparer, allocator)
    }

    /// 使用给定 comparer 与 allocator 从已有集合构建树。
    ///
    /// 重复 key 保留最后一次出现的 value。
    ///
    /// # Errors
    ///
    /// 与 [`Self::with_comparer_in`] 相同。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{BPlusTree, OrdComparer};
    /// use std::alloc::Global;
    ///
    /// let tree = BPlusTree::<4, _, i32, i32>::from_iter_with_comparer_in(
    ///     OrdComparer::new(),
    ///     vec![(2, 20), (1, 10)],
    ///     Global,
    /// )
    /// .unwrap();
    /// assert_eq!(tree.len(), 2);
    /// ```
    pub fn from_iter_with_comparer_in(
        comparer: C,
        iter: impl IntoIterator<Item = (K, V)>,
        allocator: Cfg::Allocator,
    ) -> Result<Self, TreeError> {
        let mut tree = Self::with_comparer_in(comparer, allocator)?;
        tree.extend(iter);
        Ok(tree)
    }
}

/// 删除核心逻辑。
///
/// `query_cmp` 用于与查询对象比较，`key_cmp` 用于节点内部排序。
#[allow(clippy::too_many_arguments)]
fn remove_entry_with_comparer_<const M: usize, K, V, Q, TyCmp, C, Cfg>(
    data_arena: &mut SlabList<(K, V), Cfg>,
    node_arena: &mut SlabList<TreeNode<M, K, V>, Cfg>,
    tree_root: &mut Option<TreeNodePtr<M, K, V>>,
    first_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    last_leaf: &mut Option<TreeNodePtr<M, K, V>>,
    len: &mut usize,
    query: &Q,
    query_cmp: &TyCmp,
    key_cmp: &C,
    allocator: &Cfg::Allocator,
) -> Option<(K, V)>
where
    Q: ?Sized,
    TyCmp: TrComparer<K, Q>,
    C: TrComparer<K>,
    Cfg: TrConfig,
{
    let root = (*tree_root)?;

    let query_cmp = QuerySlotComparer::new_(query_cmp);
    let leaf_ptr = find_leaf_(root, query, &query_cmp);

    let found = {
        // SAFETY: 叶子指针由树维护；调用期间 &mut 字段保证没有其他别名。
        match unsafe { node_ref_(leaf_ptr) }.as_leaf_() {
            Option::Some(leaf) => find_in_leaf_(leaf, query, &query_cmp).ok(),
            Option::None => Option::None,
        }
    };
    let (pos, data_ptr) = found?;

    let removed = match unsafe { node_mut_(leaf_ptr) }.as_leaf_mut_() {
        Option::Some(leaf) => leaf.order_arr_.try_remove_at(pos),
        Option::None => Option::None,
    };
    let (removed_ptr, ()) = removed?;
    debug_assert_eq!(removed_ptr, data_ptr);

    let leaf_max = node_max_(leaf_ptr);
    let node_cmp = SlotPairKeyComparer::new_(key_cmp);
    rebalance_after_remove_(
        node_arena, tree_root, first_leaf, last_leaf, leaf_ptr, leaf_max, &node_cmp, allocator,
    );

    *len -= 1;
    Option::Some(data_arena.remove(data_ptr, allocator))
}

/// 基于 Comparer 定序的 B+ Tree。
///
/// 泛型参数：
///
/// - `M`：每个节点的最大项数，必须至少为 2；
/// - `C`：定义 `K` 排序关系的 comparer 类型；
/// - `K` / `V`：键与值类型，`K` 不需要实现 `Ord`；
/// - `Cfg`：实现 [`TrConfig`] 的配置类型，同时指定单 Chunk 字节数和 allocator
///   类型；默认 [`DefaultSlabConfig`]（64 KiB + [`Global`]）。
///
/// 树自身持有 `Cfg::Allocator` 类型的 allocator 实例，数据 arena 与节点 arena
/// 共用它；`SlabList` 只在分配/释放 Chunk 时借用 `&Cfg::Allocator`，因此
/// allocator 不需要实现 `Clone`。B+ Tree 的 data arena 与 node arena 使用同一个
/// `Cfg::CHUNK_SIZE`。
///
/// 自定义 Chunk 大小只需替换最后一个泛型参数，例如：
///
/// ```
/// use bplustree_cmp::{BPlusTree, OrdComparer, SlabListConfig};
///
/// let mut tree: BPlusTree<4, OrdComparer<u64>, u64, u64, SlabListConfig<4096>> =
///     BPlusTree::new().unwrap();
/// tree.insert(1, 10);
/// assert_eq!(tree.get(1), Some((&1, &10)));
/// ```
///
/// 构造入口：
///
/// - `K: Ord` 时可用 [`BPlusTree::new`] / [`BPlusTree::new_in`]；
/// - 自定义 comparer 可用 [`BPlusTree::with_comparer`] /
///   [`BPlusTree::with_comparer_in`]。
pub struct BPlusTree<const M: usize, C, K, V = (), Cfg = DefaultSlabConfig>
where
    C: TrComparer<K>,
    Cfg: TrConfig,
{
    len_: usize,
    comparer_: C,
    tree_root_: Option<TreeNodePtr<M, K, V>>,
    first_leaf_: Option<TreeNodePtr<M, K, V>>,
    last_leaf_: Option<TreeNodePtr<M, K, V>>,
    allocator_: Cfg::Allocator,
    data_arena_: SlabList<(K, V), Cfg>,
    node_arena_: SlabList<TreeNode<M, K, V>, Cfg>,
}

impl<const M: usize, C, K, V, Cfg> Drop for BPlusTree<M, C, K, V, Cfg>
where
    C: TrComparer<K>,
    Cfg: TrConfig,
{
    fn drop(&mut self) {
        // SlabList 不持有 allocator，必须在 allocator 仍然存活时归还 Chunk。
        self.data_arena_.clear(&self.allocator_);
        self.node_arena_.clear(&self.allocator_);
    }
}

/// 插入冲突信息。
///
/// - [`BPlusTree::try_insert_with_comparer`] 在冲突时不调用 factory，因此
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

impl<const M: usize, C, K, V, Cfg> Extend<(K, V)> for BPlusTree<M, C, K, V, Cfg>
where
    C: TrComparer<K>,
    Cfg: TrConfig,
{
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (key, value) in iter {
            let _ = self.insert(key, value);
        }
    }
}

/// 使用 [`OrdComparer`] 与 [`Global`] allocator 从 `(K, V)` 迭代器构建树。
///
/// 重复 key 保留最后出现的 value。若当前配置无法容纳 `(K, V)` 或树节点，
/// 本实现会 panic；需要可失败版本时使用 [`BPlusTree::from_iter_in`]。
impl<const M: usize, K, V, Cfg> FromIterator<(K, V)> for BPlusTree<M, OrdComparer<K>, K, V, Cfg>
where
    K: Ord,
    Cfg: TrConfig<Allocator = Global>,
{
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        match Self::from_iter_in(iter, Global) {
            Ok(tree) => tree,
            Err(err) => panic!("使用 OrdComparer 构建 BPlusTree 失败: {err}"),
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
/// use bplustree_cmp::BPlusTree;
///
/// let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
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

/// 返回叶子链中 `(leaf_ptr, idx)` 的下一个数据位置。
fn next_position_<const M: usize, K, V>(
    leaf_ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> Option<DataPosition<M, K, V>> {
    // SAFETY: leaf_ptr 由树维护，迭代器借用树保证叶子存活。
    let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
    if idx + 1 < leaf.len_() {
        Option::Some((leaf_ptr, idx + 1))
    } else {
        leaf.next_sibl_.map(|next| (next, 0))
    }
}

/// 返回叶子链中 `(leaf_ptr, idx)` 的前一个数据位置。
fn prev_position_<const M: usize, K, V>(
    leaf_ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> Option<DataPosition<M, K, V>> {
    if idx > 0 {
        return Option::Some((leaf_ptr, idx - 1));
    }

    // SAFETY: leaf_ptr 由树维护，迭代器借用树保证叶子存活。
    let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
    let prev_ptr = leaf.prev_sibl_?;
    // SAFETY: prev_sibl_ 指向仍然存活的叶子。
    let prev = unsafe { node_ref_(prev_ptr) }.as_leaf_()?;
    Some((prev_ptr, prev.len_().checked_sub(1)?))
}

/// 子范围不可变迭代器。
///
/// 由 [`BPlusTree::range`] / [`BPlusTree::range_with_comparer`] 创建。范围
/// 首尾在构造时已经定位，迭代只沿叶子链移动，不需要工作栈或堆分配。
pub struct RangeIter<'a, const M: usize, K, V = ()> {
    /// 下一个从前端访问的位置。
    front_: Option<DataPosition<M, K, V>>,
    /// 下一个从后端访问的位置。
    back_: Option<DataPosition<M, K, V>>,
    _mark_: PhantomData<&'a (K, V)>,
}

impl<'a, const M: usize, K, V> RangeIter<'a, M, K, V> {
    /// 创建一个空范围迭代器。
    fn empty_() -> Self {
        RangeIter {
            front_: Option::None,
            back_: Option::None,
            _mark_: PhantomData,
        }
    }
}

impl<'a, const M: usize, K, V> Iterator for RangeIter<'a, M, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        let (leaf_ptr, idx) = self.front_?;

        // SAFETY: 位置由树维护；迭代器借用树，叶子和数据在 'a 内保持存活。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        let (data_ptr, ()) = leaf.order_arr_.get_ref_at(idx).ok()?;
        // SAFETY: data_ptr 指向 'a 内仍然存活的数据。
        let pair = unsafe { data_pair_(*data_ptr) };

        if self.front_ == self.back_ {
            self.front_ = Option::None;
            self.back_ = Option::None;
        } else {
            self.front_ = next_position_(leaf_ptr, idx);
        }
        Option::Some(pair)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match (self.front_, self.back_) {
            (Option::None, _) => (0, Option::Some(0)),
            (Option::Some(front), Option::Some(back)) if front == back => (1, Option::Some(1)),
            _ => (1, Option::None),
        }
    }
}

impl<const M: usize, K, V> DoubleEndedIterator for RangeIter<'_, M, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let (leaf_ptr, idx) = self.back_?;

        // SAFETY: 位置由树维护；迭代器借用树，叶子和数据在 'a 内保持存活。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        let (data_ptr, ()) = leaf.order_arr_.get_ref_at(idx).ok()?;
        // SAFETY: data_ptr 指向 'a 内仍然存活的数据。
        let pair = unsafe { data_pair_(*data_ptr) };

        if self.front_ == self.back_ {
            self.front_ = Option::None;
            self.back_ = Option::None;
        } else {
            self.back_ = prev_position_(leaf_ptr, idx);
        }
        Option::Some(pair)
    }
}

impl<const M: usize, K, V> FusedIterator for RangeIter<'_, M, K, V> {}

/// 子范围可变迭代器。
///
/// 由 [`BPlusTree::range_mut`] / [`BPlusTree::range_mut_with_comparer`] 创建。
/// 只能修改 value，key 与树结构保持不变。
pub struct RangeIterMut<'a, const M: usize, K, V = ()> {
    /// 下一个从前端访问的位置。
    front_: Option<DataPosition<M, K, V>>,
    /// 范围最后一个位置；前端越过它时迭代结束。
    back_: Option<DataPosition<M, K, V>>,
    _mark_: PhantomData<&'a mut (K, V)>,
}

impl<'a, const M: usize, K, V> RangeIterMut<'a, M, K, V> {
    /// 创建一个空范围可变迭代器。
    fn empty_() -> Self {
        RangeIterMut {
            front_: Option::None,
            back_: Option::None,
            _mark_: PhantomData,
        }
    }
}

impl<'a, const M: usize, K, V> Iterator for RangeIterMut<'a, M, K, V> {
    type Item = (&'a K, &'a mut V);

    fn next(&mut self) -> Option<Self::Item> {
        let (leaf_ptr, idx) = self.front_?;

        // SAFETY: 位置由树维护；可变迭代器独占树，叶子和数据在 'a 内保持存活。
        let leaf = unsafe { node_ref_(leaf_ptr) }.as_leaf_()?;
        let (data_ptr, ()) = leaf.order_arr_.get_ref_at(idx).ok()?;
        // SAFETY: 每个数据项只会被交出一次，且 &mut 树保证独占访问。
        let pair = unsafe { data_pair_mut_(*data_ptr) };

        if self.front_ == self.back_ {
            self.front_ = Option::None;
            self.back_ = Option::None;
        } else {
            self.front_ = next_position_(leaf_ptr, idx);
        }
        Option::Some(pair)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match (self.front_, self.back_) {
            (Option::None, _) => (0, Option::Some(0)),
            (Option::Some(front), Option::Some(back)) if front == back => (1, Option::Some(1)),
            _ => (1, Option::None),
        }
    }
}

impl<const M: usize, K, V> FusedIterator for RangeIterMut<'_, M, K, V> {}

/// 按 key 升序迭代 [`BPlusTree`] 的可变迭代器。
///
/// 只能修改 value；key 与树结构保持不变。迭代沿叶子链前进，不进行额外堆分配。
pub struct IterMut<'a, const M: usize, K, V = ()> {
    next_leaf_: Option<TreeNodePtr<M, K, V>>,
    index_: usize,
    remaining_: usize,
    _mark_: PhantomData<&'a mut (K, V)>,
}

impl<'a, const M: usize, K, V> Iterator for IterMut<'a, M, K, V> {
    type Item = (&'a K, &'a mut V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let leaf_ptr = self.next_leaf_?;

            // SAFETY: 迭代器借用树，叶子和数据在 'a 内保持存活。
            let node = unsafe { node_ref_(leaf_ptr) };
            let Option::Some(leaf) = node.as_leaf_() else {
                self.next_leaf_ = Option::None;
                return Option::None;
            };

            if self.index_ < leaf.order_arr_.len() {
                match leaf.order_arr_.get_ref_at(self.index_) {
                    Result::Ok((data_ptr, ())) => {
                        self.index_ += 1;
                        self.remaining_ -= 1;
                        // SAFETY: 每个数据项只会被交出一次，且 &mut 树保证独占访问。
                        return Option::Some(unsafe { data_pair_mut_(*data_ptr) });
                    }
                    Result::Err(_) => {
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

impl<const M: usize, K, V> ExactSizeIterator for IterMut<'_, M, K, V> {}

impl<const M: usize, K, V> FusedIterator for IterMut<'_, M, K, V> {}

/// 按 key 升序迭代 [`BPlusTree`] 的 key。
pub struct Keys<'a, const M: usize, K, V = ()> {
    iter_: TreeIter<'a, M, K, V>,
}

impl<'a, const M: usize, K, V> Iterator for Keys<'a, M, K, V> {
    type Item = &'a K;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter_.next().map(|(key, _)| key)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter_.size_hint()
    }
}

impl<const M: usize, K, V> ExactSizeIterator for Keys<'_, M, K, V> {}

impl<const M: usize, K, V> FusedIterator for Keys<'_, M, K, V> {}

/// 按 key 升序迭代 [`BPlusTree`] 的 value。
pub struct Values<'a, const M: usize, K, V = ()> {
    iter_: TreeIter<'a, M, K, V>,
}

impl<'a, const M: usize, K, V> Iterator for Values<'a, M, K, V> {
    type Item = &'a V;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter_.next().map(|(_, value)| value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter_.size_hint()
    }
}

impl<const M: usize, K, V> ExactSizeIterator for Values<'_, M, K, V> {}

impl<const M: usize, K, V> FusedIterator for Values<'_, M, K, V> {}

/// 按 key 升序迭代 [`BPlusTree`] 的可变 value。
pub struct ValuesMut<'a, const M: usize, K, V = ()> {
    iter_: IterMut<'a, M, K, V>,
}

impl<'a, const M: usize, K, V> Iterator for ValuesMut<'a, M, K, V> {
    type Item = &'a mut V;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter_.next().map(|(_, value)| value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter_.size_hint()
    }
}

impl<const M: usize, K, V> ExactSizeIterator for ValuesMut<'_, M, K, V> {}

impl<const M: usize, K, V> FusedIterator for ValuesMut<'_, M, K, V> {}

/// 对 `&BPlusTree` 的迭代器。
impl<'a, const M: usize, C, K, V, Cfg> IntoIterator for &'a BPlusTree<M, C, K, V, Cfg>
where
    C: TrComparer<K>,
    Cfg: TrConfig,
{
    type Item = (&'a K, &'a V);
    type IntoIter = TreeIter<'a, M, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// 对 `&mut BPlusTree` 的迭代器。
impl<'a, const M: usize, C, K, V, Cfg> IntoIterator for &'a mut BPlusTree<M, C, K, V, Cfg>
where
    C: TrComparer<K>,
    Cfg: TrConfig,
{
    type Item = (&'a K, &'a mut V);
    type IntoIter = IterMut<'a, M, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

#[cfg(test)]
impl<const M: usize, C, K, V, Cfg> BPlusTree<M, C, K, V, Cfg>
where
    C: TrComparer<K>,
    Cfg: TrConfig,
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
        if is_root {
            assert!(node.parent_().is_none(), "根节点的父指针应为 None");
        }
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
                    assert_eq!(
                        unsafe { node_ref_(*child) }.parent_(),
                        Option::Some(ptr),
                        "孩子的父指针应指向当前节点"
                    );
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
        let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();

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
            let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
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
        let mut tree = BPlusTree::<4, _, i32, &'static str>::new().unwrap();

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
        let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
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
        let mut tree = BPlusTree::<M, _, i32, i32>::new().unwrap();
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
        let mut tree =
            BPlusTree::<4, _, User, u64>::with_comparer(UserIdCmp).expect("默认配置应当有效");
        let query_cmp = UserIdQueryCmp;

        for id in 0..64u64 {
            let result = tree.try_insert_with_comparer(&id, &query_cmp, |id| {
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
            let (user, value) = tree
                .get_with_comparer(&id, &query_cmp)
                .expect("按 id 应能查到 User");
            assert_eq!(user.id, id);
            assert_eq!(user.name, "user");
            assert_eq!(*value, id * 10);
        }

        let removed = tree
            .remove_entry_with_comparer(&32u64, &query_cmp)
            .expect("按 id 应能删除 User");
        assert_eq!(removed.0.id, 32);
        assert_eq!(removed.1, 320);
        assert!(tree.get_with_comparer(&32u64, &query_cmp).is_none());
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
        let mut tree = BPlusTree::<4, _, i32, i32>::with_comparer(RevCmp).unwrap();
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
        let mut tree = BPlusTree::<4, _, NoCloneKey, u64>::with_comparer(NoCloneCmp)
            .expect("默认配置应当有效");
        let query_cmp = NoCloneQueryCmp;

        for id in 0..256u64 {
            let result = tree
                .try_insert_with_comparer(&id, &query_cmp, |id| (NoCloneKey { id: *id }, id * 3));
            assert!(result.is_ok());
        }

        let mut addresses = Vec::new();
        for id in 0..32u64 {
            let (key, _) = tree.get_with_comparer(&id, &query_cmp).expect("key 应存在");
            addresses.push((id, key as *const NoCloneKey));
        }

        for id in 256..1024u64 {
            let result = tree
                .try_insert_with_comparer(&id, &query_cmp, |id| (NoCloneKey { id: *id }, id * 3));
            assert!(result.is_ok());
        }

        for (id, address) in addresses {
            let (key, value) = tree.get_with_comparer(&id, &query_cmp).expect("key 应存在");
            assert_eq!(key as *const NoCloneKey, address);
            assert_eq!(*value, id * 3);
        }
        tree.check_invariants_();
    }

    /// 测试目标：验证 `from_iter_with_comparer` 与 `Extend` 的构建语义。
    /// Test goal: Verify the construction semantics of `from_iter_with_comparer` and `Extend`.
    ///
    /// 测试手段：用 `from_iter_with_comparer` 从含重复 key 的集合构建树，再 `extend` 新元素。
    /// Test method: Build a tree with `from_iter_with_comparer` from a collection containing
    /// duplicate keys, then `extend` it with new elements.
    ///
    /// 判定标准：重复 key 保留最后 value；extend 后长度和查询正确；迭代有序。
    /// Assertions: Duplicate keys keep the last value; length and lookups are correct
    /// after extend; iteration is ordered.
    #[test]
    fn test_from_iter_and_extend() {
        let items = vec![(3, "three"), (1, "one"), (2, "two"), (1, "uno")];
        let mut tree = BPlusTree::<4, _, i32, &'static str>::from_iter_with_comparer(
            OrdComparer::new(),
            items,
        )
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
        let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
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

    /// 测试目标：验证 `try_insert_with_comparer` 只在确实插入时调用 factory。
    /// Test goal: Verify that `try_insert_with_comparer` calls the factory only when an
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
        let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
        let cmp = OrdComparer::new();
        let calls = std::cell::Cell::new(0usize);

        let result = tree.try_insert_with_comparer(&1, &cmp, |_| {
            calls.set(calls.get() + 1);
            (1, 10)
        });
        assert!(result.is_ok());
        assert_eq!(calls.get(), 1);

        let conflict = tree
            .try_insert_with_comparer(&1, &cmp, |_| {
                calls.set(calls.get() + 1);
                (1, 99)
            })
            .unwrap_err();
        assert_eq!(calls.get(), 1);
        assert!(conflict.conflict().is_none());
        assert_eq!(conflict.existing(), (&1, &10));

        let result = tree.try_insert_with_comparer(&2, &cmp, |_| {
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
        let mut tree = BPlusTree::<4, _, String, i32>::new().unwrap();
        let cmp = StringStrCmp;

        for text in ["alpha", "beta", "gamma"] {
            let result = tree
                .try_insert_with_comparer(text, &cmp, |text| (text.to_string(), text.len() as i32));
            assert!(result.is_ok());
        }

        assert_eq!(
            tree.get_with_comparer("beta", &cmp)
                .map(|(key, value)| (key.as_str(), *value)),
            Some(("beta", 4))
        );

        if let Some((key, value)) = tree.get_mut_with_comparer("beta", &cmp) {
            assert_eq!(key.as_str(), "beta");
            *value = 40;
        } else {
            panic!("get_mut_with_comparer 应命中");
        }
        assert_eq!(
            tree.get_with_comparer("beta", &cmp)
                .map(|(_, value)| *value),
            Some(40)
        );

        assert_eq!(tree.remove_with_comparer("alpha", &cmp), Some(5));
        assert!(tree.get_with_comparer("alpha", &cmp).is_none());
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
        let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
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
            let mut tree =
                BPlusTree::<4, _, DropKey, DropValue>::with_comparer(DropKeyCmp).unwrap();

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
                    .remove_entry_with_comparer(&id, &query_cmp)
                    .expect("按 id 应能删除");
                drop(pair);
            }
            assert_eq!(counter.get(), 86);
        }

        assert_eq!(counter.get(), 256);
    }
}
