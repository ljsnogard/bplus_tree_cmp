//! BPlusTreeCmp: comparer-first B+ tree and B tree containers.
//!
//! Unlike `std::collections::BTreeMap`, `K` does not need to implement `Ord`.
//! The only requirement for tree operations is that the supplied comparator can
//! compare two keys and returns an `Ordering` consistent with the tree's order.
//!
//! 索引结构不复制 `K`：B+ Tree 的索引节点只保存句柄，B Tree 的节点直接保存
//! 唯一的 `(K, V)`；查询对象也可以与 `K` 不同。
//!
//! `Comparer`、`SlabList` 和通用的 `OrderedArray` 位于 crate 根层，B+ Tree
//! 专有实现收拢在 `bplus_tree` 模块中，B Tree 专有实现收拢在 `btree`
//! 模块中；两种树的节点定义分别位于各自模块的 `node_` 子模块。
//!
//! `SlabList`、`OrderedArray`、`SlotPtr` 等保持 crate 内部；两种树都暴露最后
//! 一个泛型参数 `A = Global`，用于指定内置 `SlabList` 的底层 allocator，
//! allocator 实例由树自己持有。树构造失败时返回公开的 [`TreeError`]。

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod bplus_tree;
pub mod btree;
mod comparer_;
mod ordered_arr_;
mod slab_list_;
mod tree_error_;

pub use bplus_tree::{BPlusTree, ConflictInfo, TreeIter};
pub use btree::{BTree, ConflictInfo as BTreeConflictInfo, TreeIter as BTreeIter};
pub use comparer_::{OrdComparer, TrComparer};
pub use tree_error_::TreeError;
