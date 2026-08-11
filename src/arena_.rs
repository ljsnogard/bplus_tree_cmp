use core::{
    cmp,
    marker::PhantomData,
    mem::ManuallyDrop,
};

use slab::Slab;

use crate::comparer_::TrComparer;

pub struct Arena<T> {
    slab_: ManuallyDrop<Slab<T>>,
}

impl<T> Arena<T> {
    pub const fn new() -> Self {
        Arena { slab_: ManuallyDrop::new(Slab::new()) }
    }

    pub fn insert(&mut self, data: T) -> Idx<T> {
        Idx::new(self.slab_.insert(data))
    }

    pub fn finalize(mut self) {
        todo!()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Idx<T> {
    index_: usize,
    _t_: PhantomData<T>,
}

impl<T> Idx<T> {
    const fn new(id: usize) -> Self {
        Idx {
            index_: id,
            _t_: PhantomData,
        }
    }

    pub fn get<'a, 'i>(&'i self, arena: &'a Arena<T>) -> &'i T 
    where
        'a: 'i,
    {
        &arena.slab_[self.index_]
    }

    pub fn get_mut<'a, 'i>(&'i mut self, arena: &'a mut Arena<T>) -> &'i mut T
    where
        'a: 'i,
    {
        &mut arena.slab_[self.index_]
    }
}

/// 同构定序，两个要定序的目标位于同一个 Arena
pub struct HomoIdxComparer<'a, T, C>
where
    C: TrComparer<T>,
{
    comparer_: &'a C,
    arena_: &'a Arena<T>,
}

impl<'a, T, C> HomoIdxComparer<'a, T, C>
where
    C: TrComparer<T>,
{
    pub const fn new(comparer: &'a C, arena: &'a Arena<T>) -> Self {
        HomoIdxComparer {
            comparer_: comparer,
            arena_: arena,
        }
    }
}

impl<'a, T, C> TrComparer<Idx<T>> for HomoIdxComparer<'a, T, C>
where
    C: TrComparer<T>,
{
    fn compare(&self, a: &Idx<T>, b: &Idx<T>) -> cmp::Ordering {
        let lhs = a.get(self.arena_);
        let rhs = b.get(self.arena_);
        let cmp = self.comparer_;
        cmp.compare(lhs, rhs)
    }
}

/// 异构定序，两个要定序的目标位于不同的 Arena
pub struct HeterIdxComparer<'a, L, R, C>
where
    C: TrComparer<L, R>,
{
    comparer_: &'a C,
    arena_l_: &'a Arena<L>,
    arena_r_: &'a Arena<R>,
}

impl<'a, L, R, C> HeterIdxComparer<'a, L, R, C>
where
    C: TrComparer<L, R>,
{
    pub const fn new(
        comparer: &'a C,
        arena_lhs: &'a Arena<L>,
        arena_rhs: &'a Arena<R>,
    ) -> Self {
        HeterIdxComparer {
            comparer_: comparer,
            arena_l_: arena_lhs,
            arena_r_: arena_rhs,
        }
    }
}

impl<'a, L, R, C> TrComparer<Idx<L>, Idx<R>> for HeterIdxComparer<'a, L, R, C>
where
    C: TrComparer<L, R>,
{
    fn compare(&self, l: &Idx<L>, r: &Idx<R>) -> cmp::Ordering {
        let lhs = l.get(self.arena_l_);
        let rhs = r.get(self.arena_r_);
        let cmp = self.comparer_;
        cmp.compare(lhs, rhs)
    }
}
