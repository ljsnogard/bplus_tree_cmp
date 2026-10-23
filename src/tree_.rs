use alloc::alloc::Global;

use core::{alloc::Allocator, borrow::Borrow};

use crate::{
    comparer_::TrComparer,
    node_::{TreeNode, TreeNodePtr},
    slab_list_::{SlabList, SlabListError},
};

pub struct BPlusTree<const M: usize, C, K, V = ()>
where
    C: TrComparer<K>,
{
    len_: usize,
    comparer_: C,
    tree_root_: Option<TreeNodePtr<M, K, V>>,
    data_arena_: SlabList<(K, V)>,
    node_arena_: SlabList<TreeNode<M, K, V>>,
}

impl<const M: usize, C, K, V> BPlusTree<M, C, K, V>
where
    C: TrComparer<K>,
{
    pub const fn degree(&self) -> usize {
        M
    }

    /// Creates an empty B-tree using `comparer` as its key comparator.
    ///
    /// # Errors
    ///
    /// 使用默认 `SlabListConfig<DEFAULT_CHUNK_BYTES>` 创建数据与节点
    /// `SlabList`；若默认 Chunk 无法容纳至少一个元素，返回
    /// [`SlabListError`]。
    pub fn new_in(comparer: C) -> Result<Self, SlabListError> {
        Ok(Self {
            len_: 0,
            comparer_: comparer,
            tree_root_: Option::None,
            data_arena_: SlabList::new(Global)?,
            node_arena_: SlabList::new(Global)?,
        })
    }

    /// Returns the number of entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.len_
    }

    /// Returns whether the map contains no entries.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len_ == 0
    }

    /// Returns a reference to the comparator.
    #[inline]
    pub fn comparer(&self) -> &C {
        &self.comparer_
    }

    pub fn first(&self) -> Option<(&K, &V)> {
        todo!()
    }

    /// Returns the maximum key-value pair.
    pub fn last(&self) -> Option<&(&K, &V)> {
        todo!()
    }

    /// Returns a reference to the value corresponding to `key`.
    pub fn get<Q>(&self, query: Q) -> Option<(&K, &V)>
    where
        Q: Borrow<K>,
    {
        todo!()
    }

    /// Returns a mutable reference to the value corresponding to `key`.
    pub fn get_mut<Q>(&mut self, query: Q) -> Option<(&K, &mut V)>
    where
        Q: Borrow<K>,
    {
        todo!()
    }

    pub fn get_by<'f, TyHint, TyCmp>(&'f self, hint: &TyHint, cmp: &TyCmp) -> Option<(&'f K, &'f V)>
    where
        TyCmp: TrComparer<K, TyHint>,
    {
        todo!()
    }

    pub fn get_mut_by<'f, TyHint, TyCmp>(
        &'f mut self,
        hint: &TyHint,
        cmp: &TyCmp,
    ) -> Option<(&'f K, &'f mut V)>
    where
        TyCmp: TrComparer<K, TyHint>,
    {
        todo!()
    }

    pub fn try_insert_by<'f, TyHint, TyCmp>(
        &'f mut self,
        hint: &TyHint,
        cmp: &TyCmp,
        factory: impl FnOnce(&TyHint) -> Option<(K, V)>,
    ) -> Result<(&'f K, &'f mut V), ConflictInfo<'f, K, V>>
    where
        TyCmp: TrComparer<K, TyHint>,
    {
        todo!()
    }
}

#[derive(Debug)]
pub struct ConflictInfo<'a, K, V = ()> {
    /// The mut ref to the existing item in the array
    existing: (&'a K, &'a mut V),
    /// The conflicting value that failed to insert.
    conflict: Option<(K, V)>,
}
