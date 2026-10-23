//! SlabList：按 Chunk 批量分配、直接返回元素句柄的 arena。
//!
//! 模块内部结构：
//!
//! - `config_`：Chunk 字节数配置；
//! - `error_`：创建阶段的错误类型；
//! - `ptr_`：指向槽位元素的 `SlotPtr<T>`；
//! - `chunk_`：Chunk 头、槽位布局与底层内存操作；
//! - `list_`：Chunk 链表、外部 allocator 与 arena 的公开操作。
//!
//! 对外只导出 [`SlabList`]、[`SlabListConfig`]、[`SlabListError`] 与 [`SlotPtr`]。

mod chunk_;
mod config_;
mod error_;
mod list_;
mod ptr_;

pub use config_::SlabListConfig;
pub use error_::SlabListError;
pub use list_::SlabList;
pub use ptr_::SlotPtr;
