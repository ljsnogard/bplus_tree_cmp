use core::marker::PhantomData;

use crate::{
    arena_::{Arena, HomoIdxComparer},
    comparer_::TrComparer,
    node_::{
        DataLeaf, IndexNode, RootNode, TreeNode, TreeNodeId,
    }
};

pub fn search_resursive<'f, T, H, C, const M: usize>(
    node_idx: &'f TreeNodeId<T, M>,
    arena: &'f Arena<TreeNode<T, M>>,
    hint: &'f H,
    cmp: &'f C,
) -> Result<&'f DataLeaf<T, M>, TreeNodeId<T, M>>
where
    C: TrComparer<T, H>,
{
    let node = node_idx.get(arena);
    match node {
        TreeNode::Index(index) => search_index(index, arena, hint, cmp),
        TreeNode::Leaf(leaf) => search_leaf(leaf, arena, hint, cmp),
        TreeNode::Root(root) => search_root(root, node_idx, arena, hint, cmp),
    }
}

fn search_index<'f, T, H, C, const M: usize>(
    index: &'f IndexNode<T, M>,
    arena: &'f Arena<TreeNode<T, M>>,
    hint: &'f H,
    cmp: &'f C,
) -> Result<&'f DataLeaf<T, M>, TreeNodeId<T, M>>
where
    C: TrComparer<T, H>,
{
    todo!()
}

fn search_leaf<'f, T, H, C, const M: usize>(
    leaf: &'f DataLeaf<T, M>,
    arena: &'f Arena<TreeNode<T, M>>,
    hint: &'f H,
    cmp: &'f C,
) -> Result<&'f DataLeaf<T, M>, TreeNodeId<T, M>>
where
    C: TrComparer<T, H>,
{
    todo!()
}

fn search_root<'f, T, H, C, const M: usize>(
    root: &'f RootNode<T, M>,
    root_idx: &'f TreeNodeId<T, M>,
    arena: &'f Arena<TreeNode<T, M>>,
    hint: &'f H,
    cmp: &'f C,
) -> Result<&'f DataLeaf<T, M>, TreeNodeId<T, M>>
where
    C: TrComparer<T, H>,
{
    // let idx_cmp = HomoIdxComparer::new(cmp, arena);
    // let x = root.children_.search_by(hint, &idx_cmp);
    // if let Result::Ok(id) = x {
    //     return search_resursive(id, arena, hint, cmp)
    // }
    // let Result::Err(err) = x  else{
    //     unreachable!()
    // };
    // let Option::Some(idx) = err else {
    //     return Result::Err(root_idx);
    // };
    todo!()
}

struct DataIdxHeterComparer<'a, L, R, C>
where
    C: TrComparer<L>,
{
    cmp_: &'a C,
    arena_: &'a Arena<R>,
    _mark_: PhantomData<&'a L>,
}

impl<'a, L, R, C> DataIdxHeterComparer<'a, L, R, C>
where
    C: TrComparer<L>,
{
    pub const fn new(comparer: &'a C, arena: &'a Arena<R>) -> Self {
        DataIdxHeterComparer {
            cmp_: comparer,
            arena_: arena,
            _mark_: PhantomData,
        }
    }
}
