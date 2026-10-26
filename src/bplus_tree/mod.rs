//! B+ Tree 实现。
//!
//! 本模块只包含 B+ Tree 特有的算法：
//!
//! - `tree_`：`BPlusTree` 及其公开 API、范围定位与迭代器；
//! - `search_`：从 root 下沉到叶子并定位数据。
//!
//! B+ Tree 的节点类型位于 `bplus_tree::node_`，只属于 B+ Tree。节点保存
//! 父指针，插入/删除不再使用独立的 path 栈。
//! `Comparer`、`SlabList` 和通用的 `OrderedArray` 也保持在 crate 根层。
//!
//! 本模块及其子模块的实现代码只允许通过 `SlabList` 分配堆内存，不使用
//! `Vec`、`Box` 等额外堆容器。`iter_mut` / `range` / `range_mut` 只保存固定
//! 大小的叶子链游标，因此不会为迭代引入临时堆分配。

mod node_;
mod search_;
mod tree_;

pub use tree_::{
    BPlusTree, ConflictInfo, IterMut, Keys, RangeIter, RangeIterMut, TreeIter, Values, ValuesMut,
};
