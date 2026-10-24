//! B+ Tree 插入/删除使用的祖先路径栈。
//!
//! 路径帧由 `SlabList` 分配，而不是使用 `Vec`：
//!
//! - 每个帧大小固定，只保存 `(节点指针, 孩子下标, 前驱帧指针)`；
//! - 帧按“叶子父节点 -> root”的方向组成单链表；
//! - 操作结束后把所有帧归还给 `SlabList`，后续操作复用这些槽位；
//! - 即使 `M = 2/3` 出现很深的退化链，也不会退化成任意长度的堆分配。
//!
//! 因此 path 的内存来自与树节点相同的 Chunk 分配策略，不会引入额外的
//! 长期堆碎片来源。

use crate::slab_list_::{SlabList, SlotPtr};

use super::node_::TreeNodePtr;

/// 路径栈中的一帧。
pub(crate) struct PathFrame<const M: usize, K, V> {
    /// 当前层索引节点。
    pub(crate) node_: TreeNodePtr<M, K, V>,

    /// 当前层节点在父节点中的孩子下标。
    pub(crate) child_idx_: usize,

    /// 更上一层的帧；root 帧为 `None`。
    pub(crate) prev_: Option<SlotPtr<PathFrame<M, K, V>>>,
}

/// 由 `SlabList` 槽位组成的祖先路径栈。
///
/// `Path` 本身只保存栈顶指针和长度；帧的释放需要显式调用
/// [`Path::clear_`]，并传入同一个 `SlabList`。
pub(crate) struct Path<const M: usize, K, V> {
    head_: Option<SlotPtr<PathFrame<M, K, V>>>,
}

impl<const M: usize, K, V> Path<M, K, V> {
    /// 创建一个空路径。
    pub(crate) const fn new_() -> Self {
        Path {
            head_: Option::None,
        }
    }

    /// 返回栈顶帧；栈顶是离叶子最近的索引节点。
    pub(crate) const fn head_(&self) -> Option<SlotPtr<PathFrame<M, K, V>>> {
        self.head_
    }

    /// 返回路径是否为空。
    pub(crate) const fn is_empty_(&self) -> bool {
        self.head_.is_none()
    }

    /// 压入一层索引节点。
    pub(crate) fn push_(
        &mut self,
        arena: &mut SlabList<PathFrame<M, K, V>>,
        node: TreeNodePtr<M, K, V>,
        child_idx: usize,
    ) {
        let frame = PathFrame {
            node_: node,
            child_idx_: child_idx,
            prev_: self.head_,
        };
        self.head_ = Option::Some(arena.insert(frame));
    }

    /// 把所有帧归还给 `arena`。
    ///
    /// 帧中保存的树节点指针可能已经因为合并/收缩而失效；归还过程只释放
    /// `PathFrame` 本身，不会解引用这些节点指针。
    pub(crate) fn clear_(&mut self, arena: &mut SlabList<PathFrame<M, K, V>>) {
        let mut cursor = self.head_;
        while let Option::Some(frame_ptr) = cursor {
            // 先复制前驱指针，再归还当前帧；否则 remove 后不能再读取 frame。
            let prev = frame_ptr.prev_;
            let _ = arena.remove(frame_ptr);
            cursor = prev;
        }
        self.head_ = Option::None;
    }
}
