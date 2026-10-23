//! 树搜索：从 root 下沉到叶子，并在叶子中定位数据。
//!
//! 索引项的分隔 key 是该孩子子树最大 key 的 `SlotPtr`，因此下沉规则是：
//!
//! - 取第一个 `separator >= query` 的孩子；
//! - 如果所有 separator 都小于 query，走最后一个孩子；
//! - 查询落在两个子树之间的空档时，进入后一个孩子。
//!
//! 搜索本身不复制 Key，只比较 `SlotPtr` 解引用后的实际数据。

use alloc::vec::Vec;

use crate::{
    comparer_::TrComparer,
    node_::{DataLeaf, TreeNode, TreeNodePtr},
    slab_list_::SlotPtr,
};

/// 从 root 到叶子的路径：`(索引节点, 被选中的孩子下标)`。
pub(crate) type DescentPath<const M: usize, K, V> = Vec<(TreeNodePtr<M, K, V>, usize)>;

/// 从 `root` 开始下沉，返回叶子指针以及与每一层索引节点的选择路径。
///
/// 路径元素为 `(索引节点, 被选中的孩子下标)`，按从 root 到叶子的顺序排列。
/// 插入和删除会利用该路径自底向上维护 separator。
///
/// # 安全性
///
/// `root` 必须由仍然存活的 `BPlusTree` 产生，且其孩子指针在调用期间有效。
/// 调用方必须保证树的索引节点非空；如果遇到损坏的空索引节点，本函数返回
/// 当前节点，由调用方处理。
pub(crate) fn descend_to_leaf_<const M: usize, K, V, Q, C>(
    root: TreeNodePtr<M, K, V>,
    query: &Q,
    cmp: &C,
) -> (TreeNodePtr<M, K, V>, DescentPath<M, K, V>)
where
    Q: ?Sized,
    C: TrComparer<SlotPtr<(K, V)>, Q>,
{
    let mut path = DescentPath::with_capacity(8);
    let mut current = root;

    loop {
        // SAFETY: 调用方保证 root 有效，且树结构在本次只读搜索期间保持不变；
        // 孩子句柄指向同一 node SlabList 中的存活节点。
        let node = unsafe { &*current.as_ptr() };

        let TreeNode::Index(index) = node else {
            return (current, path);
        };

        let Option::Some((pos, child)) = child_for_(index, query, cmp) else {
            // 损坏的索引节点；返回当前指针，让上层按“找不到”处理。
            return (current, path);
        };

        path.push((current, pos));
        current = child;
    }
}

/// 从 root 下沉到叶子，但不记录 path。
///
/// 只读查询不需要 path，因此可以避免每次查询的 `Vec` 分配。
pub(crate) fn find_leaf_<const M: usize, K, V, Q, C>(
    root: TreeNodePtr<M, K, V>,
    query: &Q,
    cmp: &C,
) -> TreeNodePtr<M, K, V>
where
    Q: ?Sized,
    C: TrComparer<SlotPtr<(K, V)>, Q>,
{
    let mut current = root;

    loop {
        // SAFETY: 调用方保证 root 有效，且树结构在本次只读搜索期间保持不变。
        let node = unsafe { &*current.as_ptr() };

        let TreeNode::Index(index) = node else {
            return current;
        };

        let Option::Some((_, child)) = child_for_(index, query, cmp) else {
            return current;
        };

        current = child;
    }
}

/// 在索引节点中选择孩子，返回 `(孩子下标, 孩子指针)`。
fn child_for_<const M: usize, K, V, Q, C>(
    index: &crate::node_::IndexNode<M, K, V>,
    query: &Q,
    cmp: &C,
) -> Option<(usize, TreeNodePtr<M, K, V>)>
where
    Q: ?Sized,
    C: TrComparer<SlotPtr<(K, V)>, Q>,
{
    let children = &index.children_;
    if children.is_empty() {
        return Option::None;
    }

    let pos = children.lower_bound_by(query, cmp);
    let pos = if pos == children.len() {
        children.len() - 1
    } else {
        pos
    };

    let (_, child) = children.get_ref_at(pos).ok()?;
    Option::Some((pos, *child))
}

/// 在叶子中查找与 `query` 比较等价的数据。
///
/// 找到时返回 `(逻辑下标, 数据句柄)`；未找到时返回 `Err(插入位置)`。
pub(crate) fn find_in_leaf_<const M: usize, K, V, Q, C>(
    leaf: &DataLeaf<M, K, V>,
    query: &Q,
    cmp: &C,
) -> Result<(usize, SlotPtr<(K, V)>), usize>
where
    Q: ?Sized,
    C: TrComparer<SlotPtr<(K, V)>, Q>,
{
    let pos = leaf.order_arr_.binary_search_by(query, cmp)?;
    match leaf.order_arr_.get_ref_at(pos) {
        Ok((key, _)) => Ok((pos, *key)),
        Err(pos) => Err(pos),
    }
}
