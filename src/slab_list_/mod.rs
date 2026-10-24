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
//! 本模块是 crate 私有实现；向 crate 内其他模块导出 [`SlabList`]、
//! [`SlabListError`]、[`SlotPtr`] 以及使用内置配置的 [`DefaultSlabList`] 别名。
//! `ChunkLayout`、`TrConfig` 等保持为子模块内部细节。

mod chunk_;
mod config_;
mod error_;
mod list_;
mod ptr_;

use config_::{DEFAULT_CHUNK_BYTES, SlabListConfig};
use list_::SlabList;

/// 使用内置默认配置的 `SlabList` 别名。
///
/// `A` 是底层 allocator 的类型；单 Chunk 大小固定为
/// [`DEFAULT_CHUNK_BYTES`]。allocator 实例由树类型持有，并在分配/释放
/// Chunk 时以 `&A` 的形式借给 `SlabList`。
pub(crate) type DefaultSlabList<T, A> = SlabList<T, SlabListConfig<DEFAULT_CHUNK_BYTES, A>>;

pub use error_::SlabListError;
pub use ptr_::SlotPtr;
