//! B+ Tree 实现。
//!
//! 本模块只包含 B+ Tree 特有的结构：
//!
//! - `tree_`：`BPlusTree` 及其公开 API；
//! - `node_`：`IndexNode` / `DataLeaf` / `TreeNode`；
//! - `search_`：从 root 下沉到叶子并定位数据；
//! - `path_`：插入/删除使用的 SlabList 路径帧。
//!
//! `Comparer`、`SlabList` 和通用的 `OrderedArray` 保持在 crate 根层，
//! 以便后续 B Tree 等其他索引结构复用。

mod node_;
mod path_;
mod search_;
mod tree_;

pub use tree_::{BPlusTree, ConflictInfo, TreeIter};
