//! B Tree 实现。
//!
//! 本模块只包含 B Tree 特有的算法：
//!
//! - `tree_`：`BTree` 及其公开 API；
//! - `search_`：查找与首尾；
//! - `insert_`：插入、节点分裂与上移；
//! - `remove_`：删除、后继替换、借位、合并与根收缩；
//! - `iter_`：基于父指针的中序/范围迭代器与字段级可变访问。
//!
//! B Tree 的节点类型位于 `btree::node_`，只属于 B Tree。
//! `Comparer`、`SlabList` 和通用的 `OrderedArray` 也保持在 crate 根层。
//!
//! 本模块及其子模块的实现代码只允许通过 `SlabList` 分配堆内存，不使用
//! `Vec`、`Box` 等额外堆容器。`iter_mut` / `range` / `range_mut` 只保存固定
//! 大小的节点游标；可变迭代通过字段级裸指针访问 `value_`，不与同一节点中
//! 其他已交出的 `&mut V` 产生别名。

mod insert_;
mod iter_;
mod node_;
mod remove_;
mod search_;
mod tree_;

pub use iter_::{IterMut, Keys, RangeIter, RangeIterMut, TreeIter, Values, ValuesMut};
pub use tree_::{BTree, ConflictInfo};
