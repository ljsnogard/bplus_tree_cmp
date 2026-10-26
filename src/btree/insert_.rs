//! B 树插入：节点分裂与向上传播。
//!
//! 插入采用“满节点 + 待插入项”的分裂方式，避免为节点数组引入 `M + 1` 的
//! 临时容量：
//!
//! - 叶子已满时，把待插入项与原有项合并成一个逻辑序列，中间项上移，
//!   左右两半分别留在原节点和新节点；
//! - 父节点未满时直接插入上移项；父节点已满时把上移项当作新的 pending 项，
//!   继续向上分裂；
//! - root 分裂时创建新的内部 root。
//!
//! 待插入项可能被上移到祖先节点，因此返回值会跟踪它最终所在的位置，
//! 供 `try_insert*` 直接返回 `(&K, &mut V)`。

use core::cmp::Ordering;

use crate::{
    comparer_::TrComparer,
    slab_list_::{SlabList, TrConfig},
};

use super::node_::{
    Node, NodeArray, NodeEntry, NodeValue, TreeNodePtr, node_len_, node_mut_, node_parent_,
    node_ref_, node_set_parent_, reparent_children_, split_mid_,
};

/// 待插入项在分裂后的落点。
enum PendingPlace<const M: usize, K, V> {
    /// 已经位于 `node_` 的第 `idx_` 项。
    InNode(TreeNodePtr<M, K, V>, usize),
    /// 就在上移项 [`SplitOutcome::moved_`] 中，尚未插入父节点。
    InMoved,
}

/// 一次分裂的结果。
struct SplitOutcome<const M: usize, K, V> {
    /// 上移的中间项；其右孩子已经指向 `right_`，可以插入父节点。
    moved_: NodeEntry<M, K, V>,
    /// 新产生的右节点。
    right_: TreeNodePtr<M, K, V>,
    /// 待插入项的落点。
    pending_: PendingPlace<M, K, V>,
}

/// 一次递归插入的结果。
enum InsertOutcome<const M: usize, K, V> {
    /// 待插入项已经插入 `node_` 的第 `idx_` 项。
    Inserted {
        node_: TreeNodePtr<M, K, V>,
        idx_: usize,
    },
    /// 树中已经存在等价 key；携带其位置。
    Found {
        node_: TreeNodePtr<M, K, V>,
        idx_: usize,
    },
    /// 当前节点发生分裂，父节点需要接收上移项。
    Split(SplitOutcome<M, K, V>),
}

/// 项在树中的位置：节点指针与节点内逻辑下标。
type Located<const M: usize, K, V> = (TreeNodePtr<M, K, V>, usize);

/// 把新数据项插入 B 树。
///
/// 调用方必须已经确认树中不存在与 `pair` 等价的 key；本函数仍会在内部
/// 节点命中时返回 `Err(位置)` 作为保护。
pub(crate) fn insert_pair_into_tree_<const M: usize, K, V, C, Cfg>(
    nodes: &mut SlabList<Node<M, K, V>, Cfg>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    pair: (K, V),
    cmp: &C,
    allocator: &Cfg::Allocator,
) -> Result<Located<M, K, V>, Located<M, K, V>>
where
    C: TrComparer<K>,
    Cfg: TrConfig,
{
    let pending = (pair.0, NodeValue::new_(pair.1, Option::None));

    let Option::Some(root_ptr) = *root else {
        let mut node = Node::new_leaf_();
        let result = node.entries_.try_insert_pair_by(pending, cmp);
        debug_assert!(result.is_succ());
        let ptr = nodes.insert(node, allocator);
        *root = Option::Some(ptr);
        return Ok((ptr, 0));
    };

    match insert_rec_(nodes, root_ptr, pending, cmp, allocator) {
        InsertOutcome::Inserted { node_, idx_ } => Ok((node_, idx_)),
        InsertOutcome::Found { node_, idx_ } => Err((node_, idx_)),
        InsertOutcome::Split(split) => {
            let old_root = root_ptr;
            let mut new_root = Node::new_index_(old_root);
            {
                let result = new_root.entries_.try_insert_pair_by(split.moved_, cmp);
                debug_assert!(result.is_succ());
            }
            let ptr = nodes.insert(new_root, allocator);
            *root = Option::Some(ptr);
            node_set_parent_(old_root, Option::Some(ptr));
            node_set_parent_(split.right_, Option::Some(ptr));

            match split.pending_ {
                PendingPlace::InNode(node_, idx_) => Ok((node_, idx_)),
                PendingPlace::InMoved => Ok((ptr, 0)),
            }
        }
    }
}

/// 从 `node_ptr` 开始递归插入一项。
fn insert_rec_<const M: usize, K, V, C, Cfg>(
    nodes: &mut SlabList<Node<M, K, V>, Cfg>,
    node_ptr: TreeNodePtr<M, K, V>,
    pending: NodeEntry<M, K, V>,
    cmp: &C,
    allocator: &Cfg::Allocator,
) -> InsertOutcome<M, K, V>
where
    C: TrComparer<K>,
    Cfg: TrConfig,
{
    let (is_leaf, len) = {
        // SAFETY: node_ptr 由树维护，本块只读取结构信息。
        let node = unsafe { node_ref_(node_ptr) };
        (node.is_leaf_(), node.len_())
    };

    if is_leaf {
        if len < M {
            let (pos, hit) = {
                // SAFETY: node_ptr 由树维护；本块内没有其他引用。
                let node = unsafe { node_mut_(node_ptr) };
                let pos = node.entries_.lower_bound_by(&pending.0, cmp);
                let hit = match node.entries_.get_ref_at(pos) {
                    Ok((existing_key, _)) => {
                        cmp.compare(existing_key, &pending.0) == Ordering::Equal
                    }
                    Err(_) => false,
                };
                if !hit {
                    let result = node.entries_.try_insert_pair_by(pending, cmp);
                    debug_assert!(result.is_succ());
                }
                (pos, hit)
            };

            if hit {
                return InsertOutcome::Found {
                    node_: node_ptr,
                    idx_: pos,
                };
            }
            return InsertOutcome::Inserted {
                node_: node_ptr,
                idx_: pos,
            };
        }

        return InsertOutcome::Split(split_node_with_pending_(
            nodes, node_ptr, pending, cmp, allocator,
        ));
    }

    let (pos, hit) = {
        // SAFETY: node_ptr 由树维护；本块只读取结构信息。
        let node = unsafe { node_ref_(node_ptr) };
        let pos = node.entries_.lower_bound_by(&pending.0, cmp);
        let hit = match node.entries_.get_ref_at(pos) {
            Ok((existing_key, _)) => cmp.compare(existing_key, &pending.0) == Ordering::Equal,
            Err(_) => false,
        };
        (pos, hit)
    };

    if hit {
        return InsertOutcome::Found {
            node_: node_ptr,
            idx_: pos,
        };
    }

    let child = {
        // SAFETY: node_ptr 由树维护；本块只读取结构信息。
        let node = unsafe { node_ref_(node_ptr) };
        node.child_at_(pos)
    };
    let Option::Some(child) = child else {
        unreachable!("非叶 B 树节点应存在第 pos 个孩子");
    };

    match insert_rec_(nodes, child, pending, cmp, allocator) {
        InsertOutcome::Inserted { node_, idx_ } => InsertOutcome::Inserted { node_, idx_ },
        InsertOutcome::Found { node_, idx_ } => InsertOutcome::Found { node_, idx_ },
        InsertOutcome::Split(split) => {
            let parent_len = node_len_(node_ptr);

            if parent_len < M {
                let moved_pos = {
                    // SAFETY: node_ptr 由树维护；本块内没有其他引用。
                    let node = unsafe { node_mut_(node_ptr) };
                    let pos = node.entries_.lower_bound_by(&split.moved_.0, cmp);
                    let result = node.entries_.try_insert_pair_by(split.moved_, cmp);
                    debug_assert!(result.is_succ());
                    pos
                };

                return match split.pending_ {
                    PendingPlace::InNode(node_, idx_) => InsertOutcome::Inserted { node_, idx_ },
                    PendingPlace::InMoved => InsertOutcome::Inserted {
                        node_: node_ptr,
                        idx_: moved_pos,
                    },
                };
            }

            let parent_split =
                split_node_with_pending_(nodes, node_ptr, split.moved_, cmp, allocator);
            let pending_ = match split.pending_ {
                PendingPlace::InNode(node_, idx_) => PendingPlace::InNode(node_, idx_),
                PendingPlace::InMoved => parent_split.pending_,
            };

            InsertOutcome::Split(SplitOutcome {
                moved_: parent_split.moved_,
                right_: parent_split.right_,
                pending_,
            })
        }
    }
}

/// 分裂一个已满节点，并把 `pending` 合并进分裂过程。
///
/// 合并后的逻辑序列有 `M + 1` 项。中间项上移，左节点保留前 `mid` 项，
/// 新右节点保留后 `M - mid` 项。`pending` 可能位于左节点、右节点，或者
/// 本身就是上移项。
fn split_node_with_pending_<const M: usize, K, V, C, Cfg>(
    nodes: &mut SlabList<Node<M, K, V>, Cfg>,
    node_ptr: TreeNodePtr<M, K, V>,
    pending: NodeEntry<M, K, V>,
    cmp: &C,
    allocator: &Cfg::Allocator,
) -> SplitOutcome<M, K, V>
where
    C: TrComparer<K>,
    Cfg: TrConfig,
{
    debug_assert_eq!(node_len_(node_ptr), M);

    let mid = split_mid_(M);
    let mut right_entries = NodeArray::new();
    let moved;
    let pending_side;

    {
        // SAFETY: node_ptr 由树维护；本块内没有其他引用。
        let node = unsafe { node_mut_(node_ptr) };
        debug_assert_eq!(node.entries_.len(), M);
        let pos = node.entries_.lower_bound_by(&pending.0, cmp);

        if pos < mid {
            // 右半来自原序列 mid..M。
            while node.entries_.len() > mid {
                let entry = match node.entries_.try_remove_last() {
                    Option::Some(entry) => entry,
                    Option::None => break,
                };
                push_right_(&mut right_entries, entry, cmp);
            }

            // 原序列第 mid - 1 项成为上移项。
            moved = match node.entries_.try_remove_last() {
                Option::Some(entry) => entry,
                Option::None => unreachable!("满节点应能取出一项"),
            };

            let result = node.entries_.try_insert_pair_by(pending, cmp);
            debug_assert!(result.is_succ());
            pending_side = PendingSide::Left(pos);
        } else if pos == mid {
            // 右半来自原序列 mid..M；pending 本身就是中间项。
            while node.entries_.len() > mid {
                let entry = match node.entries_.try_remove_last() {
                    Option::Some(entry) => entry,
                    Option::None => break,
                };
                push_right_(&mut right_entries, entry, cmp);
            }
            moved = pending;
            pending_side = PendingSide::Moved;
        } else {
            // 右半来自原序列 mid + 1..M，再插入 pending。
            while node.entries_.len() > mid + 1 {
                let entry = match node.entries_.try_remove_last() {
                    Option::Some(entry) => entry,
                    Option::None => break,
                };
                push_right_(&mut right_entries, entry, cmp);
            }

            // 原序列第 mid 项成为上移项。
            moved = match node.entries_.try_remove_last() {
                Option::Some(entry) => entry,
                Option::None => unreachable!("满节点应能取出一项"),
            };

            let result = right_entries.try_insert_pair_by(pending, cmp);
            debug_assert!(result.is_succ());
            pending_side = PendingSide::Right(pos - mid - 1);
        }
    }

    debug_assert_eq!(right_entries.len(), M - mid);

    // 内部节点上移项原本的右孩子应成为右节点的第 0 个孩子；叶子节点的
    // 右孩子为 None，因此这里统一处理两种情况。
    let right_first = moved.1.right_;
    let old_parent = node_parent_(node_ptr);
    let right_ptr = nodes.insert(
        Node {
            parent_: old_parent,
            first_child_: right_first,
            entries_: right_entries,
        },
        allocator,
    );
    reparent_children_(right_ptr);
    let moved = (
        moved.0,
        NodeValue::new_(moved.1.value_, Option::Some(right_ptr)),
    );

    let pending_ = match pending_side {
        PendingSide::Left(idx) => PendingPlace::InNode(node_ptr, idx),
        PendingSide::Right(idx) => PendingPlace::InNode(right_ptr, idx),
        PendingSide::Moved => PendingPlace::InMoved,
    };

    SplitOutcome {
        moved_: moved,
        right_: right_ptr,
        pending_,
    }
}

/// 分裂过程中 pending 项的临时落点。
enum PendingSide {
    Left(usize),
    Right(usize),
    Moved,
}

/// 把一个项插入右半数组。
fn push_right_<const M: usize, K, V, C>(
    right: &mut NodeArray<M, K, V>,
    entry: NodeEntry<M, K, V>,
    cmp: &C,
) where
    C: TrComparer<K>,
{
    let result = right.try_insert_pair_by(entry, cmp);
    debug_assert!(result.is_succ());
}
