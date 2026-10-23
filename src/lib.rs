//! BPlusTreeCmp: a B+ tree map whose ordering is defined entirely by a
//! caller-supplied comparator.
//!
//! Unlike `std::collections::BTreeMap`, `K` does not need to implement `Ord`.
//! The only requirement for tree operations is that the supplied comparator can
//! compare two keys and returns an `Ordering` consistent with the tree's order.
//!
//! 索引节点只保存 `SlotPtr<(K, V)>`，不复制 `K`；查询对象也可以与 `K` 不同。

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod comparer_;
mod node_;
mod ordered_arr_;
mod search_;
mod slab_list_;
mod tree_;

pub use comparer_::{OrdComparer, TrComparer};
pub use ordered_arr_::{
    ConflictInfo as ArrayConflictInfo, ConflictItem, Iter, OrderedArray, TryInsertResult,
};
pub use slab_list_::{
    ChunkLayout, DEFAULT_CHUNK_BYTES, MAX_SLOTS_PER_CHUNK, SlabList, SlabListConfig, SlabListError,
    SlotPtr, TrConfig,
};
pub use tree_::{BPlusTree, ConflictInfo, TreeIter};
