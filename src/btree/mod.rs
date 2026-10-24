//! B Tree 实现。
//!
//! 本模块只包含 B Tree 特有的算法：
//!
//! - `tree_`：`BTree` 及其公开 API；
//! - `search_`：查找与首尾；
//! - `insert_`：插入、节点分裂与上移；
//! - `remove_`：删除、后继替换、借位、合并与根收缩；
//! - `iter_`：基于父指针的中序迭代器。
//!
//! B Tree 的节点类型位于 `btree::node_`，只属于 B Tree。
//! `Comparer`、`SlabList` 和通用的 `OrderedArray` 也保持在 crate 根层。
//!
//! 本模块及其子模块的实现代码只允许通过 `SlabList` 分配堆内存，不使用
//! `Vec`、`Box` 等额外堆容器。

mod insert_;
mod iter_;
mod node_;
mod remove_;
mod search_;
mod tree_;

pub use iter_::TreeIter;
pub use tree_::{BTree, ConflictInfo};
