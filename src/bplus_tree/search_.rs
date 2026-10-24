//! 树搜索：从 root 下沉到叶子，并在叶子中定位数据。
//!
//! 索引项的分隔 key 是该孩子子树最大 key 的 `SlotPtr`，因此下沉规则是：
//!
//! - 取第一个 `separator >= query` 的孩子；
//! - 如果所有 separator 都小于 query，走最后一个孩子；
//! - 查询落在两个子树之间的空档时，进入后一个孩子。
//!
//! 搜索本身不复制 Key，只比较 `SlotPtr` 解引用后的实际数据。

use crate::{comparer_::TrComparer, slab_list_::SlotPtr};

use super::node_::{DataLeaf, IndexNode, TreeNode, TreeNodePtr};

/// 从 root 下沉到叶子。
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
    index: &IndexNode<M, K, V>,
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
