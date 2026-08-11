use core::{
    borrow::{Borrow, BorrowMut},
    cmp::Ordering,
    marker::PhantomData,
    mem::{ManuallyDrop},
    ops::Deref,
    ptr,
};

use crate::{
    arena_::Arena,
    comparer_::TrComparer,
    node_::{TreeNode, TreeNodeId},
};

pub struct BPlusTree<T, C, const M: usize>
where
    C: TrComparer<T>,
{
    len_: usize,
    comparer_: C,
    tree_root_: Option<TreeNodeId<T, M>>,
    data_arena_: Arena<T>,
    node_arena_: Arena<TreeNode<T, M>>,
}

impl<T, C, const M: usize> BPlusTree<T, C, M>
where
    C: TrComparer<T>,
{
    pub const fn degree(&self) -> usize { M }

    /// Creates an empty B-tree using `order` as its key comparator.
    pub const fn new_in(comparer: C) -> Self {
        Self {
            len_: 0,
            comparer_: comparer,
            tree_root_: Option::None,
            data_arena_: Arena::new(),
            node_arena_: Arena::new(),
        }
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

    pub fn first(&self) -> Option<&T> {
        todo!()
    }

    /// Returns the maximum key-value pair.
    pub fn last(&self) -> Option<&T> {
        todo!()
    }

    /// Returns a reference to the value corresponding to `key`.
    pub fn get<Q>(&self, key: Q) -> Option<&T>
    where
        Q: Borrow<T>,
    {
        todo!()
    }

    /// Returns a mutable reference to the value corresponding to `key`.
    pub fn get_mut<Q>(&mut self, key: Q) -> Option<&mut T>
    where
        Q: Borrow<T>,
    {
        todo!()
    }

    pub fn get_by<'f, TyHint, TyCmp>(
        &'f self,
        hint: &TyHint,
        cmp: &TyCmp,
    ) -> Option<&'f T>
    where
        TyCmp: TrComparer<T, TyHint>,
    {
        todo!()
    }

    pub fn get_mut_by<'f, TyHint, TyCmp>(
        &'f mut self,
        hint: &TyHint,
        cmp: &TyCmp,
    ) -> Option<&'f mut T>
    where
        TyCmp: TrComparer<T, TyHint>,
    {
        todo!()
    }

    pub fn try_insert_by<'f, TyHint, TyCmp>(
        &'f mut self,
        hint: &TyHint,
        cmp: &TyCmp,
        factory: impl FnOnce(&TyHint) -> Option<T>,
    ) -> Result<&'f T, Conflict<'f, T>>
    where
        TyCmp: TrComparer<T, TyHint>,
    {
        todo!()
    }
}

/// Conflict report when trying to insert into a BPlusTree
pub struct Conflict<'a, T> {
    item: &'a mut T,
    data: T,
}
