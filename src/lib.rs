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
//! 一个泛型参数 `Cfg: TrConfig`，用于同时指定内置 `SlabList` 的单 Chunk 大小
//! 和底层 allocator。默认配置为 [`DefaultSlabConfig`]（64 KiB + `Global`），
//! allocator 实例由树自己持有。树构造失败时返回公开的 [`TreeError`]。
//!
//! 两种树都提供与 `BTreeMap` 对应的范围与可变迭代 API：`range` / `range_mut`、
//! 异构 Comparer 版本 `range_with_comparer` / `range_mut_with_comparer`，以及
//! `iter_mut` / `keys` / `values` / `values_mut`。范围首尾在构造迭代器时一次
//! 定位，迭代状态是固定大小的游标，不使用工作栈或临时 `Vec`，因此这些 API
//! 不会带来额外的临时堆分配。

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

pub use bplus_tree::{
    BPlusTree, ConflictInfo, IterMut, Keys, RangeIter, RangeIterMut, TreeIter, Values, ValuesMut,
};
pub use btree::{
    BTree, ConflictInfo as BTreeConflictInfo, IterMut as BTreeIterMut, Keys as BTreeKeys,
    RangeIter as BTreeRangeIter, RangeIterMut as BTreeRangeIterMut, TreeIter as BTreeIter,
    Values as BTreeValues, ValuesMut as BTreeValuesMut,
};
pub use comparer_::{OrdComparer, TrComparer};
pub use slab_list_::{
    CHUNK_SIZE_1K, CHUNK_SIZE_2K, CHUNK_SIZE_4K, CHUNK_SIZE_8K, CHUNK_SIZE_16K, CHUNK_SIZE_32K,
    CHUNK_SIZE_64K, DEFAULT_CHUNK_BYTES, DefaultSlabConfig, SlabListConfig, TrConfig,
};
pub use tree_error_::TreeError;
