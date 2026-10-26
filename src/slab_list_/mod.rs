//! SlabList：按 Chunk 批量分配、直接返回元素句柄的 arena。
//!
//! 模块内部结构：
//!
//! - `config_`：`TrConfig` trait 与内置配置；
//! - `error_`：创建阶段的错误类型；
//! - `ptr_`：指向槽位元素的 `SlotPtr<T>`；
//! - `chunk_`：Chunk 头、槽位布局与底层内存操作；
//! - `list_`：Chunk 链表、外部 allocator 与 arena 的公开操作。
//!
//! `SlabList::remove` 在 Chunk 完全空闲时会尝试把它归还给 allocator：唯一
//! Chunk 保留，只有当前 Chunk 的前驱或后继也完全空闲时才摘链释放。这是一次
//! 局部的回收决策，不保证链表中绝对不存在相邻的空闲 Chunk。
//!
//! 本模块是 crate 私有实现；`SlabList`、`SlabListError`、`SlotPtr` 与
//! `ChunkLayout` 保持 crate 内部。配置抽象 [`TrConfig`]、内置配置
//! [`SlabListConfig`]、默认别名 [`DefaultSlabConfig`] 与
//! [`DEFAULT_CHUNK_BYTES`] 通过 crate 根公开给使用者。

mod chunk_;
mod config_;
mod error_;
mod list_;
mod ptr_;

pub use config_::{
    CHUNK_SIZE_1K, CHUNK_SIZE_2K, CHUNK_SIZE_4K, CHUNK_SIZE_8K, CHUNK_SIZE_16K, CHUNK_SIZE_32K,
    CHUNK_SIZE_64K, DEFAULT_CHUNK_BYTES, DefaultSlabConfig, SlabListConfig, TrConfig,
};
pub(crate) use list_::SlabList;

pub(crate) use error_::SlabListError;
pub(crate) use ptr_::SlotPtr;
