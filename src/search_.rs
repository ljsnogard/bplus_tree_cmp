use crate::{
    arena_::{Arena, Idx, HomoIdxComparer},
    comparer_::TrComparer,
    node_::{
        DataLeaf, IndexNode, RootNode, TreeNode, TreeNodeId,
    }
};

#[derive(Clone, Copy, Debug)]
pub enum PartitionPointIdx<T, const M: usize> {
    Data(Idx<T>),
    Node(TreeNodeId<T, M>),
}

/// 从某一个给定的节点开始进行递归搜索，返回第一个使 pred 返回 false 的位置。
/// 且该位置应为 DataLeaf （如果非空）或者 node_idx （如果空节点，例如当
/// 树刚刚构建，完全没有任何节点没有 Root 的情况下）
/// Pred 的语义应该
pub fn partition_point_resursive_<'f, T, H, P, const M: usize>(
    node_idx: &'f TreeNodeId<T, M>,
    arena: &'f Arena<TreeNode<T, M>>,
    hint: &'f H,
    pred: P,
) -> PartitionPointIdx<T, M>
where
    P: FnMut(&T, &H) -> bool,
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
fn partition_point_from_index_<'f, T, H, P, const M: usize>(
    index: &'f IndexNode<T, M>,
    arena: &'f Arena<TreeNode<T, M>>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPointIdx<T, M>
where
    P: FnMut(&T, &H) -> bool,
{
    todo!()
}

fn partition_point_from_leaf<'f, T, H, P, const M: usize>(
    leaf: &'f DataLeaf<T, M>,
    arena: &'f Arena<TreeNode<T, M>>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPointIdx<T, M>
where
    P: FnMut(&T, &H) -> bool,
{
    todo!()
}

fn partition_point_from_root<'f, T, H, P, const M: usize>(
    root: &'f RootNode<T, M>,
    root_idx: &'f TreeNodeId<T, M>,
    arena: &'f Arena<TreeNode<T, M>>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPointIdx<T, M>
where
    P: FnMut(&T, &H) -> bool,
{
    todo!()
}
