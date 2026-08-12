//! BTreeCmp: a B-tree map whose ordering is defined entirely by a caller-supplied comparator.
//!
//! Unlike `std::collections::BTreeMap`, `K` does not need to implement `Ord`.
//! The only requirement for tree operations is that the supplied comparator can
//! compare two keys and returns an `Ordering` consistent with the tree's order.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod arena_;
mod comparer_;
mod tree_;
mod node_;
mod ordered_arr_;
mod search_;

pub use comparer_::{OrdComparer, TrComparer};
pub use ordered_arr_::{OrderedArray, TryInsertResult, Iter};
pub use tree_::BPlusTree;

pub mod x_deps {
    pub use slab;
}

pub trait IntoPair {
    type PairType;

    fn into_pair(self) -> (Self, ())
    where
        Self: Sized,
    {
        (self, ())
    }
}

impl<T: Sized> IntoPair for T {
    type PairType = (T, ());
}