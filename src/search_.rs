use crate::{
    arena_::{Arena, Idx, HomoIdxComparer},
    comparer_::TrComparer,
    node_::{
        DataLeaf, IndexNode, RootNode, TreeNode, TreeNodeId,
    }
};

#[derive(Clone, Copy, Debug)]
pub enum PartitionPointIdx<const M: usize, K, V> {
    Data(Idx<(K, V)>),
    Node(TreeNodeId<M, K, V>),
}

/// 从某一个给定的节点开始进行递归搜索，返回第一个使 pred 返回 false 的位置。
/// 且该位置应为 DataLeaf （如果非空）或者 node_idx （如果空节点，例如当
/// 树刚刚构建，完全没有任何节点没有 Root 的情况下）
/// Pred 的语义应该
pub fn partition_point_recursive_<'f, const M: usize, K, V, H, P>(
    node_idx: &'f TreeNodeId<M, K, V>,
    arena: &'f Arena<TreeNode<M, K, V>>,
    hint: &'f H,
    pred: P,
) -> PartitionPointIdx<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    let node = node_idx.get(arena);
    match node {
        TreeNode::Index(index) =>
            partition_point_from_index_(
                index,
                arena,
                hint,
                pred,
            ),
        TreeNode::Leaf(leaf) =>
            partition_point_from_leaf(
                leaf,
                arena,
                hint,
                pred,
            ),
        TreeNode::Root(root) =>
            partition_point_from_root(
                root,
                node_idx,
                arena,
                hint,
                pred,
            ),
    }
}

/// 从 IndexNode 开始进行递归搜索，返回第一个使 pred 返回 false 的位置。
/// 且该位置应为 DataLeaf （如果非空）或者 node_idx （如果空节点）
fn partition_point_from_index_<'f, const M: usize, K, V, H, P>(
    index: &'f IndexNode<M, K, V>,
    arena: &'f Arena<TreeNode<M, K, V>>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPointIdx<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    todo!()
}

fn partition_point_from_leaf<'f, const M: usize, K, V, H, P>(
    leaf: &'f DataLeaf<M, K, V>,
    arena: &'f Arena<TreeNode<M, K, V>>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPointIdx<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    todo!()
}

fn partition_point_from_root<'f, const M: usize, K, V, H, P>(
    root: &'f RootNode<M, K, V>,
    root_idx: &'f TreeNodeId<M, K, V>,
    arena: &'f Arena<TreeNode<M, K, V>>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPointIdx<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    todo!()
}
