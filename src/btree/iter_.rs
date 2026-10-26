//! B 树中序、范围与可变迭代器。
//!
//! B Tree 的数据分散在所有层级的节点中。本实现利用节点中的父指针计算中序
//! 前驱/后继，不需要工作栈：
//!
//! - 当前项在内部节点中时，后继是它右子树的最小项；
//! - 当前项在叶子中时，后继是同叶子的下一项，或者沿父链找到第一个
//!   “当前孩子不是最后一个孩子”的祖先，取该祖先中对应的项；
//! - 范围迭代器只保存首、尾两个 `(节点, 下标)` 游标；
//! - 可变迭代器通过字段级裸指针只对 `value_` 建立 `&mut V`，允许同一节点中
//!   多个可变引用同时存活。
//!
//! 因此 B 树节点和迭代器状态都只通过树结构本身与 `SlabList` 管理，没有
//! `Vec` 或其他额外堆分配。

use core::{iter::FusedIterator, marker::PhantomData};

use super::node_::{
    Node, NodePosition, RangePositions, TreeNodePtr, child_at_raw_, child_index_, child_index_raw_,
    entry_mut_raw_, entry_ref_, node_is_leaf_raw_, node_len_raw_, node_parent_, node_parent_raw_,
    node_ref_,
};

/// 按 key 升序迭代 [`super::BTree`]。
///
/// 迭代器借用树，因此不能在迭代期间修改树。父指针由树结构维护，迭代器
/// 本身不需要额外工作栈。
pub struct TreeIter<'a, const M: usize, K, V = ()> {
    /// 当前项的节点与节点内下标；`None` 表示迭代结束。
    current_: Option<NodePosition<M, K, V>>,
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

/// 子范围不可变迭代器。
///
/// 由 [`super::BTree::range`] / [`super::BTree::range_with_comparer`] 创建。
/// 首尾位置在构造时已经定位，迭代沿父指针计算中序前驱/后继，不需要工作栈
/// 或额外堆分配。
pub struct RangeIter<'a, const M: usize, K, V = ()> {
    front_: Option<NodePosition<M, K, V>>,
    back_: Option<NodePosition<M, K, V>>,
    _mark_: PhantomData<&'a (K, V)>,
}

impl<'a, const M: usize, K, V> RangeIter<'a, M, K, V> {
    /// 由首尾位置创建范围迭代器。
    pub(crate) fn new_(positions: Option<RangePositions<M, K, V>>) -> Self {
        match positions {
            Option::Some((front, back)) => RangeIter {
                front_: Option::Some(front),
                back_: Option::Some(back),
                _mark_: PhantomData,
            },
            Option::None => RangeIter {
                front_: Option::None,
                back_: Option::None,
                _mark_: PhantomData,
            },
        }
    }
}

impl<'a, const M: usize, K, V> Iterator for RangeIter<'a, M, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        let (node_ptr, idx) = self.front_?;
        // SAFETY: 位置由树维护；迭代器借用树，节点在 'a 内保持存活。
        let pair = unsafe { entry_ref_(node_ptr, idx) };

        if self.front_ == self.back_ {
            self.front_ = Option::None;
            self.back_ = Option::None;
        } else {
            self.front_ = next_entry_(node_ptr, idx);
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
        let (node_ptr, idx) = self.back_?;
        // SAFETY: 位置由树维护；迭代器借用树，节点在 'a 内保持存活。
        let pair = unsafe { entry_ref_(node_ptr, idx) };

        if self.front_ == self.back_ {
            self.front_ = Option::None;
            self.back_ = Option::None;
        } else {
            self.back_ = prev_entry_(node_ptr, idx);
        }
        Option::Some(pair)
    }
}

impl<const M: usize, K, V> FusedIterator for RangeIter<'_, M, K, V> {}

/// 子范围可变迭代器。
///
/// 由 [`super::BTree::range_mut`] / [`super::BTree::range_mut_with_comparer`]
/// 创建。只能修改 value，key 与树结构保持不变。
pub struct RangeIterMut<'a, const M: usize, K, V = ()> {
    front_: Option<NodePosition<M, K, V>>,
    back_: Option<NodePosition<M, K, V>>,
    _mark_: PhantomData<&'a mut (K, V)>,
}

impl<'a, const M: usize, K, V> RangeIterMut<'a, M, K, V> {
    /// 由首尾位置创建范围可变迭代器。
    pub(crate) fn new_(positions: Option<RangePositions<M, K, V>>) -> Self {
        match positions {
            Option::Some((front, back)) => RangeIterMut {
                front_: Option::Some(front),
                back_: Option::Some(back),
                _mark_: PhantomData,
            },
            Option::None => RangeIterMut {
                front_: Option::None,
                back_: Option::None,
                _mark_: PhantomData,
            },
        }
    }
}

impl<'a, const M: usize, K, V> Iterator for RangeIterMut<'a, M, K, V> {
    type Item = (&'a K, &'a mut V);

    fn next(&mut self) -> Option<Self::Item> {
        let (node_ptr, idx) = self.front_?;

        // 先计算后继，再交出当前项的可变引用，避免在 &mut V 存活期间重新
        // 创建覆盖整个节点的引用。
        let at_last = self.front_ == self.back_;
        let next = if at_last {
            Option::None
        } else {
            // SAFETY: 位置由树维护，迭代器独占树。
            unsafe { next_entry_raw_(node_ptr, idx) }
        };

        // SAFETY: 每个数据项只会被交出一次，迭代器独占树。
        let pair = unsafe { entry_mut_raw_(node_ptr, idx) };

        if at_last {
            self.front_ = Option::None;
            self.back_ = Option::None;
        } else {
            self.front_ = next;
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

/// 按 key 升序迭代 [`super::BTree`] 的可变迭代器。
///
/// 只能修改 value；key 与树结构保持不变。迭代沿父指针前进，不进行额外堆分配。
pub struct IterMut<'a, const M: usize, K, V = ()> {
    current_: Option<NodePosition<M, K, V>>,
    remaining_: usize,
    _mark_: PhantomData<&'a mut (K, V)>,
}

impl<'a, const M: usize, K, V> IterMut<'a, M, K, V> {
    /// 创建一个中序可变迭代器。
    pub(crate) fn new_(root: Option<TreeNodePtr<M, K, V>>, len: usize) -> Self {
        IterMut {
            current_: root.map(leftmost_),
            remaining_: len,
            _mark_: PhantomData,
        }
    }
}

impl<'a, const M: usize, K, V> Iterator for IterMut<'a, M, K, V> {
    type Item = (&'a K, &'a mut V);

    fn next(&mut self) -> Option<Self::Item> {
        let (node_ptr, idx) = self.current_?;

        // 先计算后继，再取出当前项，理由同 RangeIterMut。
        // SAFETY: 位置由树维护，迭代器独占树。
        let next = unsafe { next_entry_raw_(node_ptr, idx) };
        // SAFETY: 每个数据项只会被交出一次，迭代器独占树。
        let pair = unsafe { entry_mut_raw_(node_ptr, idx) };

        self.current_ = next;
        self.remaining_ -= 1;
        Option::Some(pair)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining_, Option::Some(self.remaining_))
    }
}

impl<const M: usize, K, V> ExactSizeIterator for IterMut<'_, M, K, V> {}

impl<const M: usize, K, V> FusedIterator for IterMut<'_, M, K, V> {}

/// 按 key 升序迭代 [`super::BTree`] 的 key。
pub struct Keys<'a, const M: usize, K, V = ()> {
    pub(crate) iter_: TreeIter<'a, M, K, V>,
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

/// 按 key 升序迭代 [`super::BTree`] 的 value。
pub struct Values<'a, const M: usize, K, V = ()> {
    pub(crate) iter_: TreeIter<'a, M, K, V>,
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

/// 按 key 升序迭代 [`super::BTree`] 的可变 value。
pub struct ValuesMut<'a, const M: usize, K, V = ()> {
    pub(crate) iter_: IterMut<'a, M, K, V>,
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

// 迭代器只保存节点位置游标。不可变迭代器交出共享引用，因此发送或共享它要求
// `K`/`V: Sync`；可变迭代器交出独占引用，发送它要求 `K`/`V: Send`，而共享
// `&IterMut` 不能调用 `next`，因此只要求 `K`/`V: Sync`。
//
// SAFETY: 迭代器内部只保存指向树中仍然存活节点的句柄；树借用保证这些节点在
// 迭代器生命周期内不会被移动或释放，跨线程移动/共享迭代器不会引入额外别名。
unsafe impl<const M: usize, K, V> Send for TreeIter<'_, M, K, V>
where
    K: Sync,
    V: Sync,
{
}

// SAFETY: 理由同 [`TreeIter`] 的 `Send` 实现；`size_hint` 等 `&self` 操作只读。
unsafe impl<const M: usize, K, V> Sync for TreeIter<'_, M, K, V>
where
    K: Sync,
    V: Sync,
{
}

// SAFETY: 不可变范围迭代器与 [`TreeIter`] 一样只交出共享引用。
unsafe impl<const M: usize, K, V> Send for RangeIter<'_, M, K, V>
where
    K: Sync,
    V: Sync,
{
}

// SAFETY: `&RangeIter` 只用于读取游标和 `size_hint`，不修改树或数据。
unsafe impl<const M: usize, K, V> Sync for RangeIter<'_, M, K, V>
where
    K: Sync,
    V: Sync,
{
}

// SAFETY: 可变迭代器拥有对不重复 value 的独占访问；跨线程移动它等同于移动
// `&mut V`，因此要求 `K`/`V: Send`。
unsafe impl<const M: usize, K, V> Send for IterMut<'_, M, K, V>
where
    K: Send,
    V: Send,
{
}

// SAFETY: `&IterMut` 没有可用的前进方法（`next` 需要 `&mut self`），共享引用
// 不会产生对 value 的可变访问；`K`/`V: Sync` 足以保证 `size_hint` 等只读访问。
unsafe impl<const M: usize, K, V> Sync for IterMut<'_, M, K, V>
where
    K: Sync,
    V: Sync,
{
}

// SAFETY: 可变范围迭代器与 [`IterMut`] 一样拥有不重复 value 的独占访问。
unsafe impl<const M: usize, K, V> Send for RangeIterMut<'_, M, K, V>
where
    K: Send,
    V: Send,
{
}

// SAFETY: `&RangeIterMut` 只能读取游标和 `size_hint`，前进需要 `&mut self`。
unsafe impl<const M: usize, K, V> Sync for RangeIterMut<'_, M, K, V>
where
    K: Sync,
    V: Sync,
{
}

/// 返回以 `node_ptr` 为根的最左项的 `(节点, 下标)`。
fn leftmost_<const M: usize, K, V>(mut node_ptr: TreeNodePtr<M, K, V>) -> NodePosition<M, K, V> {
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
pub(crate) fn next_entry_<const M: usize, K, V>(
    node_ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> Option<NodePosition<M, K, V>> {
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

/// 返回以 `node_ptr` 为根的最右项的 `(节点, 下标)`。
fn rightmost_<const M: usize, K, V>(mut node_ptr: TreeNodePtr<M, K, V>) -> NodePosition<M, K, V> {
    loop {
        // SAFETY: node_ptr 由树维护。
        let node = unsafe { node_ref_(node_ptr) };

        if node.is_leaf_() {
            return (node_ptr, node.len_().saturating_sub(1));
        }

        let Option::Some(child) = node.last_child_() else {
            return (node_ptr, node.len_().saturating_sub(1));
        };
        node_ptr = child;
    }
}

/// 返回 `(node_ptr, idx)` 的中序前驱。
pub(crate) fn prev_entry_<const M: usize, K, V>(
    node_ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> Option<NodePosition<M, K, V>> {
    // SAFETY: node_ptr 由树维护。
    let node = unsafe { node_ref_(node_ptr) };

    // 内部项的前驱是它左子树的最大项。
    if !node.is_leaf_()
        && let Option::Some(left_child) = node.child_at_(idx)
    {
        return Option::Some(rightmost_(left_child));
    }

    if idx > 0 {
        return Option::Some((node_ptr, idx - 1));
    }

    // 已经是当前节点的第一项：沿父链寻找第一个“当前孩子不是第一个孩子”的祖先。
    let mut child = node_ptr;
    loop {
        let parent = node_parent_(child)?;
        let child_idx = child_index_(parent, child)?;
        if child_idx > 0 {
            return Option::Some((parent, child_idx - 1));
        }
        child = parent;
    }
}

/// 通过裸指针返回 `(node_ptr, idx)` 的中序后继。
///
/// # 安全性
///
/// `node_ptr` 必须指向当前树中仍然存活的节点，`idx` 必须小于节点项数。
pub(crate) unsafe fn next_entry_raw_<const M: usize, K, V>(
    node_ptr: TreeNodePtr<M, K, V>,
    idx: usize,
) -> Option<NodePosition<M, K, V>> {
    // SAFETY: 调用方保证节点存活；只读取结构与 child 指针。
    if !unsafe { node_is_leaf_raw_(node_ptr) }
        && let Option::Some(right_child) = unsafe { child_at_raw_(node_ptr, idx + 1) }
    {
        // SAFETY: right_child 属于同一棵仍然存活的树。
        return Option::Some(unsafe { leftmost_raw_(right_child) });
    }

    // SAFETY: 调用方保证节点存活。
    let len = unsafe { node_len_raw_(node_ptr) };
    if idx + 1 < len {
        return Option::Some((node_ptr, idx + 1));
    }

    let mut child = node_ptr;
    loop {
        // SAFETY: child 属于同一棵仍然存活的树。
        let parent = unsafe { node_parent_raw_(child) }?;
        // SAFETY: parent/child 均属于同一棵仍然存活的树。
        let child_idx = unsafe { child_index_raw_(parent, child) }?;
        // SAFETY: parent 存活。
        let parent_len = unsafe { node_len_raw_(parent) };
        if child_idx < parent_len {
            return Option::Some((parent, child_idx));
        }
        child = parent;
    }
}

/// 通过裸指针返回以 `node_ptr` 为根的最左项。
///
/// # 安全性
///
/// `node_ptr` 必须指向当前树中仍然存活的节点。
unsafe fn leftmost_raw_<const M: usize, K, V>(
    mut node_ptr: TreeNodePtr<M, K, V>,
) -> NodePosition<M, K, V> {
    loop {
        // SAFETY: node_ptr 属于同一棵仍然存活的树。
        if unsafe { node_is_leaf_raw_(node_ptr) } {
            return (node_ptr, 0);
        }
        // SAFETY: node_ptr 存活且内部节点至少有一个孩子。
        let Option::Some(child) = (unsafe { child_at_raw_(node_ptr, 0) }) else {
            return (node_ptr, 0);
        };
        node_ptr = child;
    }
}
