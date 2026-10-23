//! SlabList 的 Chunk 配置。

use super::{
    chunk_::{ChunkLayout, layout_for_},
    error_::SlabListError,
};

/// 单个 Chunk 的默认总内存大小：64 KiB。
pub const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;

/// SlabList 的 Chunk 配置。
///
/// 配置只记录单个 Chunk 的**总内存大小**（字节），不绑定具体的元素类型 `T`。
/// 真正的槽位数量、布局和对齐会在需要时根据 `T` 的槽布局实时计算，
/// 不作为独立字段缓存在 [`crate::SlabList`] 中：
///
/// ```text
/// chunk_bytes
///   = ChunkHeader
///   + 对齐填充
///   + slots_per_chunk * size_of::<Slot<T>>()
///   + 可能的尾部余量
/// ```
///
/// 单个 Chunk 的槽位数不会超过
/// [`SlabListConfig::MAX_SLOTS_PER_CHUNK`]（2^15）。
///
/// # Examples
///
/// ```
/// use bplustree_cmp::SlabListConfig;
///
/// let config = SlabListConfig::new(16 * 1024);
/// assert_eq!(config.chunk_bytes(), 16 * 1024);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlabListConfig {
    chunk_bytes_: usize,
}

impl SlabListConfig {
    /// 单个 Chunk 允许的最大槽位数：2^15。
    ///
    /// 每个槽尾部的 `self_idx: u16` 使用最高位作为 occupied 标记，
    /// 因此槽号只能使用低 15 位，可寻址槽数为 `0..2^15`。
    pub const MAX_SLOTS_PER_CHUNK: usize = 1 << 15;

    /// 用单 Chunk 的总内存大小（字节）创建配置。
    ///
    /// `chunk_bytes` 包含 Chunk header、对齐填充以及所有槽位。
    /// 对于具体的 `T`，这个大小是否足以容纳至少一个槽位，
    /// 要到创建 [`crate::SlabList`] 时才能确定。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::SlabListConfig;
    ///
    /// let config = SlabListConfig::new(4096);
    /// assert_eq!(config.chunk_bytes(), 4096);
    /// ```
    pub const fn new(chunk_bytes: usize) -> Self {
        SlabListConfig {
            chunk_bytes_: chunk_bytes,
        }
    }

    /// 返回单 Chunk 的总内存大小（字节）。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::SlabListConfig;
    ///
    /// let config = SlabListConfig::new(8192);
    /// assert_eq!(config.chunk_bytes(), 8192);
    /// ```
    pub const fn chunk_bytes(&self) -> usize {
        self.chunk_bytes_
    }

    /// 计算给定元素类型 `T` 的每 Chunk 槽位数。
    ///
    /// 该值由 `chunk_bytes` 与 `T` 的槽布局实时计算，不作为独立字段缓存。
    ///
    /// # Errors
    ///
    /// 与创建 [`crate::SlabList`] 时相同：Chunk 太小返回
    /// [`SlabListError::ChunkTooSmall`]，槽位数超过上限返回
    /// [`SlabListError::TooManySlots`]，布局无法表示返回
    /// [`SlabListError::LayoutOverflow`]。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::SlabListConfig;
    ///
    /// let config = SlabListConfig::new(4096);
    /// assert!(config.slots_per_chunk::<u64>().unwrap() > 0);
    /// ```
    pub fn slots_per_chunk<T>(&self) -> Result<usize, SlabListError> {
        self.chunk_layout_::<T>()
            .map(|layout| layout.slots_per_chunk_ as usize)
    }

    /// 根据具体元素类型 `T` 计算单 Chunk 的完整布局。
    ///
    /// 布局信息不会作为独立字段缓存在 `SlabList` 中；每次需要时都从配置的
    /// `chunk_bytes` 和 `T` 的槽布局重新计算。
    pub(crate) fn chunk_layout_<T>(&self) -> Result<ChunkLayout, SlabListError> {
        layout_for_::<T>(self.chunk_bytes_)
    }
}

impl Default for SlabListConfig {
    /// 使用 64 KiB 作为默认单 Chunk 大小。
    fn default() -> Self {
        SlabListConfig::new(DEFAULT_CHUNK_BYTES)
    }
}
