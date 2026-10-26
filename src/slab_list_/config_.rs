//! SlabList 的配置抽象与内置配置类型。

use core::{alloc::Allocator, marker::PhantomData};

use alloc::alloc::Global;

/// 单个 Chunk 的默认总内存大小：64 KiB。
pub const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;

/// 1 KiB Chunk 大小常量。
pub const CHUNK_SIZE_1K: usize = 1024;

/// 2 KiB Chunk 大小常量。
pub const CHUNK_SIZE_2K: usize = 2 * 1024;

/// 4 KiB Chunk 大小常量。
pub const CHUNK_SIZE_4K: usize = 4 * 1024;

/// 8 KiB Chunk 大小常量。
pub const CHUNK_SIZE_8K: usize = 8 * 1024;

/// 16 KiB Chunk 大小常量。
pub const CHUNK_SIZE_16K: usize = 16 * 1024;

/// 32 KiB Chunk 大小常量。
pub const CHUNK_SIZE_32K: usize = 32 * 1024;

/// 64 KiB Chunk 大小常量。
pub const CHUNK_SIZE_64K: usize = DEFAULT_CHUNK_BYTES;

/// 单个 Chunk 允许的最大槽位数：2^15。
///
/// 每个槽尾部的 `self_idx: u16` 使用最高位作为 occupied 标记，
/// 因此槽号只能使用低 15 位，可寻址槽数为 `0..2^15`。
pub const MAX_SLOTS_PER_CHUNK: usize = 1 << 15;

/// SlabList 与树使用的类型级配置。
///
/// `TrConfig` 把“单 Chunk 的内存大小”和“分配 Chunk 使用的 allocator 类型”
/// 合并成一个类型级配置。`BPlusTree` / `BTree` 的最后一个泛型参数就是
/// `Cfg: TrConfig`，因此使用者只需选择配置类型，就能同时指定 Chunk 大小和
/// allocator。
///
/// 配置本身是类型级的：`TrConfig` 只声明关联类型 [`TrConfig::Allocator`]
/// 和常量 [`TrConfig::CHUNK_SIZE`]，不要求实现者持有 allocator 实例。
/// allocator 实例由使用 `SlabList` 的类型（例如树）持有，并在分配/释放
/// Chunk 时以 `&Cfg::Allocator` 的形式借给 `SlabList`。
///
/// Allocator 只负责 Chunk 内存分配，与元素类型 `T` 无关。
///
/// `CHUNK_SIZE` 是单个 Chunk 的总分配字节数，不是“刚好容纳多少个元素”的
/// 槽位数。它必须至少能容纳 Chunk 头和该类型的一个槽位，否则树构造会返回
/// [`crate::TreeError`]；若计算出的槽位数超过 `2^15`，同样会返回错误。
///
/// 内置配置 [`SlabListConfig`] 允许直接使用 const 泛型选择字节数；crate 也提供
/// [`CHUNK_SIZE_1K`]、[`CHUNK_SIZE_2K`]、[`CHUNK_SIZE_4K`]、[`CHUNK_SIZE_8K`]、
/// [`CHUNK_SIZE_16K`]、[`CHUNK_SIZE_32K`]、[`CHUNK_SIZE_64K`] 作为推荐档位：
///
/// ```
/// use bplustree_cmp::{BPlusTree, OrdComparer, SlabListConfig, CHUNK_SIZE_4K};
///
/// type Tree = BPlusTree<4, OrdComparer<u64>, u64, u64, SlabListConfig<CHUNK_SIZE_4K>>;
/// let tree = Tree::new().unwrap();
/// assert!(tree.is_empty());
/// ```
///
/// 这些常量只是推荐值，`TrConfig` 本身保持开放；使用者可以按自己的分配器与
/// 元素规模实现任意 Chunk 大小。
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
    /// 配置只指定类型；allocator 实例归使用 `SlabList` 的类型所有。
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
/// `CHUNK_SIZE` 是分配字节数，不是槽位数；不同元素类型 `T` 会得到不同的
/// 槽位数。通常建议选择 2 的幂（例如 1 KiB、4 KiB、64 KiB），但这只是建议，
/// `TrConfig` 保持开放，使用者也可以自行实现自己的配置类型。
///
/// # Examples
///
/// ```
/// use bplustree_cmp::{SlabListConfig, TrConfig};
///
/// assert_eq!(SlabListConfig::<4096>::CHUNK_SIZE, 4096);
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

/// 使用默认 Chunk 大小的配置。
///
/// 通过类型参数 `A` 指定 allocator，默认 [`Global`]。等价于
/// `SlabListConfig<DEFAULT_CHUNK_BYTES, A>`。
///
/// # Examples
///
/// ```
/// use bplustree_cmp::{DefaultSlabConfig, TrConfig, DEFAULT_CHUNK_BYTES};
/// use std::alloc::Global;
///
/// assert_eq!(
///     DefaultSlabConfig::<Global>::CHUNK_SIZE,
///     DEFAULT_CHUNK_BYTES
/// );
/// ```
pub type DefaultSlabConfig<A = Global> = SlabListConfig<DEFAULT_CHUNK_BYTES, A>;
