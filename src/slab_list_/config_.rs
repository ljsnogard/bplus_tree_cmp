//! SlabList 的配置抽象与内置配置类型。

use core::{alloc::Allocator, marker::PhantomData};

use alloc::alloc::Global;

/// 单个 Chunk 的默认总内存大小：64 KiB。
pub const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;

/// 单个 Chunk 允许的最大槽位数：2^15。
///
/// 每个槽尾部的 `self_idx: u16` 使用最高位作为 occupied 标记，
/// 因此槽号只能使用低 15 位，可寻址槽数为 `0..2^15`。
pub const MAX_SLOTS_PER_CHUNK: usize = 1 << 15;

/// SlabList 的类型级配置。
///
/// `SlabList<T, C>` 的 `T` 与 `C` 是两个独立的泛型参数：`T` 决定槽位中的
/// 元素类型，`C` 决定单 Chunk 的内存大小和分配 Chunk 内存所用的 allocator
/// 类型。
///
/// 配置本身是类型级的：`TrConfig` 只声明关联类型 [`TrConfig::Allocator`]
/// 和常量 [`TrConfig::CHUNK_SIZE`]，不要求实现者持有 allocator 实例。
/// allocator 实例由 [`crate::SlabList`] 自己保存，并在构造时传入。
///
/// Allocator 只负责 Chunk 内存分配，与元素类型 `T` 无关。
///
/// # Examples
///
/// ```
/// use bplustree_cmp::{SlabListConfig, TrConfig};
///
/// assert_eq!(SlabListConfig::<4096>::CHUNK_SIZE, 4096);
/// ```
pub trait TrConfig {
    /// 分配 Chunk 内存所使用的 allocator 类型。
    ///
    /// 配置只指定类型；allocator 实例归 `SlabList` 所有。
    type Allocator: Allocator;

    /// 单个 Chunk 的总内存大小（字节）。
    const CHUNK_SIZE: usize;
}

/// 内置的类型级配置。
///
/// 通过 const 泛型参数指定单 Chunk 字节数，通过类型参数 `A` 指定 allocator
/// 类型；默认 allocator 为 [`Global`]。这个类型本身不需要实例化，只作为
/// `SlabList` 的泛型参数使用：
///
/// ```text
/// SlabList<T, SlabListConfig<CHUNK_SIZE, A>>
/// ```
///
/// # Examples
///
/// ```
/// use bplustree_cmp::{SlabList, SlabListConfig};
/// use std::alloc::Global;
///
/// let arena = SlabList::<u64, SlabListConfig<4096>>::new(Global).unwrap();
/// assert_eq!(arena.chunk_bytes(), 4096);
/// ```
pub struct SlabListConfig<const CHUNK_SIZE: usize, A = Global> {
    _marker_: PhantomData<A>,
}

impl<const CHUNK_SIZE: usize, A> TrConfig for SlabListConfig<CHUNK_SIZE, A>
where
    A: Allocator,
{
    type Allocator = A;

    const CHUNK_SIZE: usize = CHUNK_SIZE;
}
