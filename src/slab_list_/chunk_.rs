//! SlabList 的 Chunk 内存布局与底层操作。
//!
//! 一个 Chunk 使用一次 `alloc` 分配，布局如下：
//!
//! ```text
//! +----------------------+  <- Chunk 基址（也是 ChunkHeader 的地址）
//! | ChunkHeader          |
//! |  prev / next         |
//! |  free_head           |
//! |  free_count          |
//! |  capacity            |
//! +----------------------+  <- slot_offset（按 Slot<T> 对齐）
//! | Slot<T> #0           |
//! |  value: MaybeUninit  |
//! |  self_idx: u16       |
//! |  next_free: u16      |
//! +----------------------+
//! | Slot<T> #1           |
//! | ...                  |
//! +----------------------+
//! ```
//!
//! `Slot<T>` 的 `value_` 位于槽位起始地址，因此指向 `T` 的指针同时也是
//! 槽位基址。`self_idx_` 的低 15 位保存槽号，最高位保存 occupied 标记；
//! `next_free_` 保存空闲链表中的下一个槽号，`u16::MAX` 表示链表结束。

use core::{
    alloc::{Allocator, Layout},
    mem::{MaybeUninit, align_of, size_of},
    ptr::{self, NonNull},
};

use alloc::alloc::handle_alloc_error;

use super::{config_::SlabListConfig, error_::SlabListError};

/// `self_idx_` 的 occupied 标记位。
pub(crate) const OCCUPIED_BIT: u16 = 1 << 15;

/// 从 `self_idx_` 中提取槽号时使用的掩码。
pub(crate) const SLOT_INDEX_MASK: u16 = OCCUPIED_BIT - 1;

/// 空闲链表结束标记。
pub(crate) const NIL_SLOT: u16 = u16::MAX;

/// Chunk 头，位于每个 Chunk 分配内存的起始位置。
#[repr(C)]
pub(crate) struct ChunkHeader {
    /// 前一个 Chunk；链表头部为 null。
    pub(crate) prev_: *mut ChunkHeader,

    /// 后一个 Chunk；链表尾部为 null。
    pub(crate) next_: *mut ChunkHeader,

    /// 空闲链表头部的槽号；没有空闲槽时为 [`NIL_SLOT`]。
    pub(crate) free_head_: u16,

    /// 当前空闲槽数量。
    pub(crate) free_count_: u16,

    /// 本 Chunk 的槽位总数。
    pub(crate) capacity_: u16,
}

/// 一个槽位：元素数据在前，两个 u16 元数据在后。
#[repr(C)]
pub(crate) struct Slot<T> {
    /// 尚未初始化或正在使用的元素。
    pub(crate) value_: MaybeUninit<T>,

    /// 低 15 位是槽号，最高位是 occupied 标记。
    pub(crate) self_idx_: u16,

    /// 空闲链表中的下一个槽号；[`NIL_SLOT`] 表示没有下一个空闲槽。
    pub(crate) next_free_: u16,
}

/// 由 [`SlabListConfig`] 与具体元素类型 `T` 计算出的 Chunk 布局。
///
/// 该值只在需要时临时计算，不缓存在 `SlabList` 中。
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChunkLayout {
    /// 单 Chunk 的分配布局。
    pub(crate) layout_: Layout,

    /// 槽位数组相对于 Chunk 基址的偏移。
    pub(crate) slot_offset_: usize,

    /// 每个 Chunk 的槽位数。
    pub(crate) slots_per_chunk_: u16,
}

/// 把 `value` 向上对齐到 `align`。
///
/// `align` 必须是 2 的幂。返回值使用 `Option` 表达加法溢出。
const fn align_up_(value: usize, align: usize) -> Option<usize> {
    let mask = align - 1;
    match value.checked_add(mask) {
        Some(padded) => Some(padded & !mask),
        None => None,
    }
}

/// 返回槽位数组相对于 Chunk 基址的偏移。
///
/// 偏移会按 `Slot<T>` 的对齐要求向上取整。Chunk 分配本身按
/// `max(align_of::<ChunkHeader>(), align_of::<Slot<T>>())` 对齐，
/// 因此基址加上该偏移后仍然满足 `Slot<T>` 的对齐要求。
pub(crate) fn slot_offset_<T>() -> Option<usize> {
    align_up_(size_of::<ChunkHeader>(), align_of::<Slot<T>>())
}

/// 返回 Chunk 分配所需的整体对齐。
pub(crate) fn chunk_align_<T>() -> usize {
    align_of::<ChunkHeader>().max(align_of::<Slot<T>>())
}

/// 根据配置的 Chunk 字节数计算布局、槽位数组偏移与槽位数。
///
/// # Errors
///
/// - Chunk 字节数不足以容纳 header、对齐填充和至少一个槽位时返回
///   [`SlabListError::ChunkTooSmall`]。
/// - 槽位数超过 [`SlabListConfig::MAX_SLOTS_PER_CHUNK`] 时返回
///   [`SlabListError::TooManySlots`]。
/// - 布局无法表示时返回 [`SlabListError::LayoutOverflow`]。
pub(crate) fn layout_for_<T>(chunk_bytes: usize) -> Result<ChunkLayout, SlabListError> {
    let slot_offset = slot_offset_::<T>().ok_or(SlabListError::LayoutOverflow)?;
    let stride = size_of::<Slot<T>>();
    debug_assert!(stride > 0, "Slot<T> 至少包含两个 u16 元数据");

    let available = chunk_bytes
        .checked_sub(slot_offset)
        .ok_or(SlabListError::ChunkTooSmall)?;
    let slots = available / stride;

    if slots == 0 {
        return Err(SlabListError::ChunkTooSmall);
    }
    if slots > SlabListConfig::MAX_SLOTS_PER_CHUNK {
        return Err(SlabListError::TooManySlots);
    }

    let layout = Layout::from_size_align(chunk_bytes, chunk_align_::<T>())
        .map_err(|_| SlabListError::LayoutOverflow)?;

    Ok(ChunkLayout {
        layout_: layout,
        slot_offset_: slot_offset,
        slots_per_chunk_: slots as u16,
    })
}

/// 返回 Chunk 中槽位数组的起始地址。
///
/// # Safety
///
/// - `chunk` 必须指向一个按 `layout_for_::<T>` 分配并初始化的 Chunk。
/// - `slot_offset` 必须等于 `slot_offset_::<T>()`，且该 Chunk 至少有一个槽位。
pub(crate) unsafe fn slots_base_<T>(chunk: *mut ChunkHeader, slot_offset: usize) -> *mut Slot<T> {
    // SAFETY: 调用者保证 chunk 指向有效的 Chunk 分配，slot_offset 在分配范围内
    // 且按 Slot<T> 对齐。Chunk 基址本身按 Slot<T> 对齐，因此相加后仍然对齐。
    unsafe { (chunk as *mut u8).add(slot_offset).cast::<Slot<T>>() }
}

/// 返回 Chunk 中第 `idx` 个槽位的地址。
///
/// # Safety
///
/// - `chunk`、`slot_offset` 的要求同 [`slots_base_`]。
/// - `idx` 必须小于该 Chunk 的 `capacity_`。
pub(crate) unsafe fn slot_ptr_at_<T>(
    chunk: *mut ChunkHeader,
    slot_offset: usize,
    idx: u16,
) -> *mut Slot<T> {
    // SAFETY: 调用者保证 idx 在槽位范围内。
    unsafe { slots_base_::<T>(chunk, slot_offset).add(idx as usize) }
}

/// 返回槽位中 `self_idx_` 字段的地址。
///
/// # Safety
///
/// `slot` 必须指向一个已经开始初始化、且元数据仍然有效的 `Slot<T>`。
pub(crate) unsafe fn slot_self_idx_ptr_<T>(slot: *mut Slot<T>) -> *mut u16 {
    // SAFETY: 调用者保证 slot 有效；addr_of_mut! 不会创建对字段的引用。
    unsafe { ptr::addr_of_mut!((*slot).self_idx_) }
}

/// 由槽位地址与槽号反推出所属 Chunk 的基址。
///
/// 槽位在 Chunk 内连续排列，因此：
///
/// ```text
/// slot0 = slot - idx * size_of::<Slot<T>>()
/// chunk = slot0 - slot_offset
/// ```
///
/// # Safety
///
/// - `slot` 必须指向一个有效 Chunk 中的有效槽位。
/// - `idx` 必须等于该槽位 `self_idx_` 的低 15 位。
/// - `slot_offset` 必须等于 `slot_offset_::<T>()`。
pub(crate) unsafe fn chunk_of_slot_<T>(
    slot: *mut Slot<T>,
    idx: u16,
    slot_offset: usize,
) -> *mut ChunkHeader {
    // SAFETY: 调用者保证 idx 是当前槽位在 Chunk 内的真实下标，
    // 因此 slot.sub(idx) 仍位于同一 Chunk 的槽位数组中。
    let slot0 = unsafe { slot.sub(idx as usize) };
    // SAFETY: slot0 是槽位数组起始地址，减去 slot_offset 得到 Chunk 基址。
    unsafe { (slot0 as *mut u8).sub(slot_offset).cast::<ChunkHeader>() }
}

/// 使用外部 allocator 分配并初始化一个 Chunk。
///
/// 初始化后的 Chunk 拥有一个包含全部槽位的空闲链表，链表顺序为
/// `0 -> 1 -> ... -> capacity - 1 -> NIL`。
///
/// # Safety
///
/// - `chunk_layout` 必须来自 [`layout_for_::<T>`] 的成功计算。
/// - `allocator` 必须能够分配 `chunk_layout.layout_` 描述的布局。
///
/// 分配失败时按全局 allocator 的错误处理器处理（通常是 abort）。
pub(crate) unsafe fn alloc_chunk_<T, A>(
    allocator: &A,
    chunk_layout: ChunkLayout,
) -> *mut ChunkHeader
where
    A: Allocator,
{
    let layout = chunk_layout.layout_;

    // SAFETY: 调用者保证 allocator 与 layout 匹配。
    // allocate 返回的指针按 layout.align 对齐，因而同时满足 ChunkHeader
    // 与 Slot<T> 的对齐要求。
    let allocation = match allocator.allocate(layout) {
        Ok(allocation) => allocation,
        Err(_) => handle_alloc_error(layout),
    };
    let base = allocation.as_ptr() as *mut u8;

    let chunk = base.cast::<ChunkHeader>();

    // SAFETY:
    // - base 指向一块至少 layout.size() 字节的有效内存。
    // - slot_offset + slots * size_of::<Slot<T>>() <= layout.size()，
    //   这个不等式由 layout_for_ 的计算保证。
    // - base 按 chunk_align_::<T>() 对齐，slot_offset 按 Slot<T> 对齐，
    //   因此槽位数组的每个元素都满足 Slot<T> 的对齐要求。
    unsafe {
        chunk.write(ChunkHeader {
            prev_: ptr::null_mut(),
            next_: ptr::null_mut(),
            free_head_: 0,
            free_count_: chunk_layout.slots_per_chunk_,
            capacity_: chunk_layout.slots_per_chunk_,
        });

        let slots_ptr = base.add(chunk_layout.slot_offset_).cast::<Slot<T>>();
        let capacity = chunk_layout.slots_per_chunk_ as usize;
        for i in 0..capacity {
            let next_free = if i + 1 < capacity {
                (i + 1) as u16
            } else {
                NIL_SLOT
            };
            slots_ptr.add(i).write(Slot {
                value_: MaybeUninit::uninit(),
                self_idx_: i as u16,
                next_free_: next_free,
            });
        }
    }

    chunk
}

/// 使用外部 allocator 释放一个由 [`alloc_chunk_`] 分配的 Chunk。
///
/// # Safety
///
/// - `chunk` 必须由同一个 allocator 的 `alloc_chunk_` 返回，且尚未被释放。
/// - `layout` 必须与分配时使用的 layout 完全一致。
pub(crate) unsafe fn dealloc_chunk_<A>(allocator: &A, chunk: *mut ChunkHeader, layout: Layout)
where
    A: Allocator,
{
    // SAFETY: 调用者保证 chunk 与 layout 的匹配关系。
    unsafe {
        allocator.deallocate(NonNull::new_unchecked(chunk.cast::<u8>()), layout);
    }
}
