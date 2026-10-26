//! B 树搜索：从 root 下沉到目标项或叶子。
//!
//! 搜索规则与 B+ Tree 不同：B 树的每个节点都保存实际数据，因此在内部节点
//! 命中 key 时可以直接返回，不需要继续下沉到叶子。
//!
//! - 在当前节点的 `entries_` 中二分查找；
//! - 命中时返回 `(节点, 逻辑下标)`；
//! - 未命中时进入插入位置对应的左孩子：第 `pos` 个孩子；
//! - 叶子未命中表示树中不存在该 key。
//!
//! 数组中的 key 就是实际 `K`，因此搜索直接使用用户提供的 Comparer，不需要
//! 额外的 `Entry` 适配层。

use core::{
    cmp::Ordering,
    ops::{Bound, RangeBounds},
};

use crate::comparer_::TrComparer;

use super::{
    iter_::{next_entry_, prev_entry_},
    node_::{NodePosition, RangePositions, TreeNodePtr, entry_ref_, node_ref_},
};

/// 在树中查找与 `query` 等价的项。
///
/// 找到时返回 `(节点指针, 节点内逻辑下标)`；未找到时返回 `None`。
pub(crate) fn find_entry_<const M: usize, K, V, Q, C>(
    root: TreeNodePtr<M, K, V>,
    query: &Q,
    cmp: &C,
) -> Option<NodePosition<M, K, V>>
where
    Q: ?Sized,
    C: TrComparer<K, Q>,
{
    let mut current = root;

    loop {
        // SAFETY: 调用方保证 root 有效，且树结构在本次只读搜索期间保持不变；
        // 孩子句柄指向同一 node SlabList 中的存活节点。
        let node = unsafe { node_ref_(current) };

        match node.entries_.binary_search_by(query, cmp) {
            Ok(idx) => return Option::Some((current, idx)),
            Err(pos) => {
                // 叶子没有孩子：`child_at_` 返回 None，表示未命中。
                current = node.child_at_(pos)?;
            }
        }
    }
}

/// 返回树中最小项的位置。
pub(crate) fn first_entry_<const M: usize, K, V>(
    root: TreeNodePtr<M, K, V>,
) -> Option<NodePosition<M, K, V>> {
    let mut current = root;

    loop {
        // SAFETY: 调用方保证节点指针由树维护且在调用期间保持存活。
        let node = unsafe { node_ref_(current) };

        if node.is_leaf_() {
            return if node.len_() > 0 {
                Option::Some((current, 0))
            } else {
                Option::None
            };
        }

        current = node.first_child_?;
    }
}

/// 返回树中最大项的位置。
pub(crate) fn last_entry_<const M: usize, K, V>(
    root: TreeNodePtr<M, K, V>,
) -> Option<NodePosition<M, K, V>> {
    let mut current = root;

    loop {
        // SAFETY: 调用方保证节点指针由树维护且在调用期间保持存活。
        let node = unsafe { node_ref_(current) };

        if node.is_leaf_() {
            let len = node.len_();
            return if len > 0 {
                Option::Some((current, len - 1))
            } else {
                Option::None
            };
        }

        current = node.last_child_()?;
    }
}

/// 返回第一个满足 `key >= query`（`strict == false`）或 `key > query`
/// （`strict == true`）的项。
pub(crate) fn bound_entry_<const M: usize, K, V, Q, C>(
    root: TreeNodePtr<M, K, V>,
    query: &Q,
    cmp: &C,
    strict: bool,
) -> Option<NodePosition<M, K, V>>
where
    Q: ?Sized,
    C: TrComparer<K, Q>,
{
    let mut current = root;

    loop {
        // SAFETY: 调用方保证 root 有效，且树结构在本次只读搜索期间保持不变。
        let node = unsafe { node_ref_(current) };
        let pos = if strict {
            node.entries_.upper_bound_by(query, cmp)
        } else {
            node.entries_.lower_bound_by(query, cmp)
        };

        if !node.is_leaf_() {
            // `pos == len` 时 child_at_ 返回最后一个孩子。
            current = node.child_at_(pos)?;
            continue;
        }

        if pos < node.len_() {
            return Option::Some((current, pos));
        }

        let len = node.len_();
        return if len > 0 {
            next_entry_(current, len - 1)
        } else {
            Option::None
        };
    }
}

/// 判断 key 是否已经越过范围的上界。
fn past_end_<K, Q, C>(key: &K, end: Bound<&Q>, cmp: &C) -> bool
where
    Q: ?Sized,
    C: TrComparer<K, Q>,
{
    match end {
        Bound::Unbounded => false,
        Bound::Included(end) => cmp.compare(key, end) == Ordering::Greater,
        Bound::Excluded(end) => cmp.compare(key, end) != Ordering::Less,
    }
}

/// 判断 key 是否位于范围下界之前。
fn before_start_<K, Q, C>(key: &K, start: Bound<&Q>, cmp: &C) -> bool
where
    Q: ?Sized,
    C: TrComparer<K, Q>,
{
    match start {
        Bound::Unbounded => false,
        Bound::Included(start) => cmp.compare(key, start) == Ordering::Less,
        Bound::Excluded(start) => cmp.compare(key, start) != Ordering::Greater,
    }
}

/// 计算一个范围在 B 树中的首、尾项位置。
///
/// 首项是不低于下界的第一个项；尾项是不高于上界的最后一个项。若树中没有
/// 任何项落在区间内，返回 `None`。
pub(crate) fn range_positions_<const M: usize, K, V, Q, C, R>(
    root: Option<TreeNodePtr<M, K, V>>,
    range: &R,
    cmp: &C,
) -> Option<RangePositions<M, K, V>>
where
    Q: ?Sized,
    C: TrComparer<K, Q>,
    R: RangeBounds<Q>,
{
    let root = root?;
    let start = range.start_bound();
    let end = range.end_bound();

    let front = match start {
        Bound::Unbounded => first_entry_(root)?,
        Bound::Included(query) => bound_entry_(root, query, cmp, false)?,
        Bound::Excluded(query) => bound_entry_(root, query, cmp, true)?,
    };

    let back = match end {
        Bound::Unbounded => last_entry_(root)?,
        Bound::Included(query) => {
            // 第一个大于上界的项的前驱，就是最后一个不大于上界的项。
            match bound_entry_(root, query, cmp, true) {
                Option::Some(position) => prev_entry_(position.0, position.1)?,
                Option::None => last_entry_(root)?,
            }
        }
        Bound::Excluded(query) => {
            // 第一个不小于上界的项的前驱，就是最后一个小于上界的项。
            match bound_entry_(root, query, cmp, false) {
                Option::Some(position) => prev_entry_(position.0, position.1)?,
                Option::None => last_entry_(root)?,
            }
        }
    };

    // SAFETY: front/back 来自当前存活树的搜索结果。
    let front_key = unsafe { entry_ref_(front.0, front.1) }.0;
    // SAFETY: 同上。
    let back_key = unsafe { entry_ref_(back.0, back.1) }.0;

    if past_end_(front_key, end, cmp) || before_start_(back_key, start, cmp) {
        Option::None
    } else {
        Option::Some((front, back))
    }
}
