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

use crate::comparer_::TrComparer;

use super::node_::{TreeNodePtr, node_ref_};

/// 在树中查找与 `query` 等价的项。
///
/// 找到时返回 `(节点指针, 节点内逻辑下标)`；未找到时返回 `None`。
pub(crate) fn find_entry_<const M: usize, K, V, Q, C>(
    root: TreeNodePtr<M, K, V>,
    query: &Q,
    cmp: &C,
) -> Option<(TreeNodePtr<M, K, V>, usize)>
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
) -> Option<(TreeNodePtr<M, K, V>, usize)> {
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
) -> Option<(TreeNodePtr<M, K, V>, usize)> {
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
