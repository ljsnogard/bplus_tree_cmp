//! 基于 Chunk 链表的 SlabList arena。

// 内部 arena 保留完整诊断/构造接口供测试与后续使用。
#![allow(dead_code)]

use core::{
    marker::PhantomData,
    ptr::{self, NonNull},
};

use super::{
    chunk_::{
        ChunkHeader, ChunkLayout, NIL_SLOT, OCCUPIED_BIT, SLOT_INDEX_MASK, Slot, alloc_chunk_,
        chunk_of_slot_, dealloc_chunk_, slot_ptr_at_, slot_self_idx_ptr_, slots_base_,
    },
    config_::{DEFAULT_CHUNK_BYTES, SlabListConfig, TrConfig},
    error_::SlabListError,
    ptr_::SlotPtr,
};

/// 一个按 Chunk 批量预留槽位的 arena。
///
/// 与逐元素分配的容器不同，`SlabList` 每次需要扩容时会一次性分配一个
/// Chunk；Chunk 的全部槽位共享同一片内存，Chunk 头本身也构建在这片内存上。
///
/// 多个 Chunk 通过 `prev_` / `next_` 指针组成双向链表：
///
/// - 有空闲槽位的 Chunk 尽量靠近链表头部；
/// - 已经写满的 Chunk 会被移到链表尾部；
/// - 某个 Chunk 归还槽位后，通常会被移到链表头部，以便后续插入优先复用它；
///   若它因此变成完全空闲且相邻 Chunk 也完全空闲，则可能被直接释放。
///
/// 这是“空闲最多的 Chunk 在头部”的近似策略，而不是按 `free_count` 严格排序。
/// 链表头部始终是可分配的 Chunk（如果还存在可分配 Chunk）。
///
/// 每个槽位的尾部有一对 `u16`：
///
/// - `self_idx`：低 15 位是槽号，最高位是 occupied 标记；
/// - `next_free`：空闲链表中的下一个槽号，`u16::MAX` 表示链表结束。
///
/// 因此槽位可以乱序归还，归还的槽位会压入所属 Chunk 的空闲链表头部。
///
/// 当一个 Chunk 因 `remove` 变成完全空闲时，`SlabList` 会尝试归还它的内存：
/// 唯一 Chunk 保留；否则若前驱或后继也完全空闲，则摘链并调用 allocator 释放。
/// 这是一次局部决策，不保证所有相邻空闲 Chunk 都在同一次操作中被回收。
///
/// `T` 与配置类型 `C` 是两个独立的泛型参数：`T` 是槽位中的元素类型，`C` 是
/// 类型级配置，决定单 Chunk 的 const 字节数和 allocator 类型。配置本身不持有
/// allocator 实例，`SlabList` 也不长期保存 allocator：分配新 Chunk 和释放
/// Chunk 时由调用方把 `&C::Allocator` 借给 `SlabList`。
///
/// 单 Chunk 的布局、槽位偏移和槽位数不作为独立字段缓存在 `SlabList` 中，
/// 而是由 `C::CHUNK_SIZE` 结合 `T` 在需要时计算。
///
/// `SlabList` 不保存存活元素数量、总容量或 Chunk 数量等使用统计信息；
/// 它只负责管理 Chunk 链表和槽位的分配/归还。
///
/// # Examples
///
/// ```text
/// use crate::slab_list_::{SlabList, SlabListConfig};
/// use std::alloc::Global;
///
/// let mut arena = SlabList::<u64, SlabListConfig<4096>>::new().unwrap();
///
/// let a = arena.insert(10, &Global);
/// let b = arena.insert(20, &Global);
///
/// assert_eq!(*a, 10);
/// assert_eq!(*b, 20);
/// assert_eq!(arena.remove(a, &Global), 10);
/// assert_eq!(*b, 20);
/// arena.clear(&Global);
/// ```
pub struct SlabList<T, C = SlabListConfig<DEFAULT_CHUNK_BYTES>>
where
    C: TrConfig,
{
    /// 链表头部；为空表示还没有分配任何 Chunk。
    head_: *mut ChunkHeader,

    /// 链表尾部。
    tail_: *mut ChunkHeader,

    /// 表明 `SlabList<T, C>` 拥有 `T`，并参与 drop check。
    _marker_: PhantomData<(Slot<T>, C)>,
}

impl<T, C> SlabList<T, C>
where
    C: TrConfig,
{
    /// 创建一个空的 `SlabList`。
    ///
    /// `C` 是类型级配置：它决定 `CHUNK_SIZE` 和 allocator 类型。allocator
    /// 实例不归 `SlabList` 所有，而是在 [`SlabList::insert`] 和
    /// [`SlabList::clear`] 时以 `&C::Allocator` 的形式传入。
    ///
    /// 创建时不会立即分配 Chunk；第一次 `insert` 时才分配第一个 Chunk。
    ///
    /// # Errors
    ///
    /// 当 `C::CHUNK_SIZE` 无法容纳至少一个 `Slot<T>` 时返回
    /// [`SlabListError::ChunkTooSmall`]；当计算出的槽位数超过
    /// [`MAX_SLOTS_PER_CHUNK`](super::config_::MAX_SLOTS_PER_CHUNK) 时返回
    /// [`SlabListError::TooManySlots`]；布局无法表示时返回
    /// [`SlabListError::LayoutOverflow`]。
    ///
    /// # Examples
    ///
    /// ```text
    /// use crate::slab_list_::{SlabList, SlabListConfig};
    /// use std::alloc::Global;
    ///
    /// let arena = SlabList::<u64, SlabListConfig<4096>>::new().unwrap();
    /// assert_eq!(arena.chunk_bytes(), 4096);
    /// ```
    pub fn new() -> Result<Self, SlabListError> {
        ChunkLayout::for_type::<T>(C::CHUNK_SIZE)?;

        Ok(SlabList {
            head_: ptr::null_mut(),
            tail_: ptr::null_mut(),
            _marker_: PhantomData,
        })
    }

    /// 返回单 Chunk 的总内存大小（字节）。
    pub const fn chunk_bytes(&self) -> usize {
        C::CHUNK_SIZE
    }

    /// 返回每个 Chunk 的槽位数。
    ///
    /// 该值由 `C::CHUNK_SIZE` 和 `T` 的布局实时计算，不作为独立字段缓存在
    /// `SlabList` 中。
    pub fn slots_per_chunk(&self) -> usize {
        self.chunk_layout_().slots_per_chunk()
    }

    /// 从 `C::CHUNK_SIZE` 和 `T` 计算 Chunk 布局。
    ///
    /// `new` 已经验证过配置，因此这里不应失败。
    fn chunk_layout_(&self) -> ChunkLayout {
        match ChunkLayout::for_type::<T>(C::CHUNK_SIZE) {
            Ok(layout) => layout,
            Err(_) => unreachable!("SlabList 创建时已校验 Chunk 配置"),
        }
    }

    /// 在 arena 中写入一个元素，并返回指向它的句柄。
    ///
    /// 如果链表头部 Chunk 已满，会使用 `allocator` 分配一个新 Chunk 并放到
    /// 链表头部。元素写入后地址保持稳定，直到它被 `remove` 或
    /// [`SlabList::clear`] 释放。
    ///
    /// # Panics
    ///
    /// 本方法不会主动 panic。底层分配失败时按全局 allocator 的错误处理器处理。
    ///
    /// # Examples
    ///
    /// ```text
    /// use crate::slab_list_::{SlabList, SlabListConfig};
    /// use std::alloc::Global;
    ///
    /// let mut arena = SlabList::<u64, SlabListConfig<4096>>::new().unwrap();
    /// let a = arena.insert(1, &Global);
    /// let b = arena.insert(2, &Global);
    ///
    /// assert_eq!(*a, 1);
    /// assert_eq!(*b, 2);
    /// ```
    pub fn insert(&mut self, value: T, allocator: &C::Allocator) -> SlotPtr<T> {
        let chunk_layout = self.chunk_layout_();
        let chunk = self.ensure_free_chunk_(chunk_layout, allocator);

        // SAFETY:
        // - chunk 由 ensure_free_chunk_ 返回，非空且 free_count_ > 0。
        // - free_head_ 是空闲链表头，因此是合法槽号。
        // - slot_ptr_at_ 的 idx 在 capacity_ 范围内。
        // - Slot<T> 的 value_ 位于槽位起始地址，slot.cast::<T>() 满足对齐，
        //   且该槽位当前未被占用，可以写入。
        unsafe {
            let idx = (*chunk).free_head_;
            debug_assert!(idx != NIL_SLOT);
            debug_assert!((idx as usize) < (*chunk).capacity_ as usize);

            let slot = slot_ptr_at_::<T>(chunk, chunk_layout.slot_offset_, idx);
            let next_free = (*slot).next_free_;

            // 先弹出空闲链表头，再写入元素，避免元素初始化后仍被视为空闲。
            (*chunk).free_head_ = next_free;
            (*chunk).free_count_ -= 1;

            let value_ptr = slot.cast::<T>();
            value_ptr.write(value);
            (*slot).self_idx_ = OCCUPIED_BIT | idx;
            (*slot).next_free_ = NIL_SLOT;

            if (*chunk).free_count_ == 0 {
                self.move_to_tail_(chunk);
            }

            SlotPtr::from_raw_(NonNull::new_unchecked(value_ptr))
        }
    }

    /// 取回并归还一个槽位，返回其中原来的元素。
    ///
    /// 槽位会被压入所属 Chunk 的空闲链表头部。若该 Chunk 仍有存活元素，它会被
    /// 移到整个 Chunk 链表的头部，以便后续 `insert` 优先复用。
    ///
    /// 当该 Chunk 在本 `remove` 后完全空闲时，会尝试归还它的内存：
    ///
    /// - 如果它是链表中唯一的 Chunk，保留它，避免下次插入重新分配；
    /// - 否则，只要前驱或后继也是完全空闲的 Chunk，就把当前 Chunk 从链表摘下
    ///   并通过 `allocator` 归还内存；
    /// - 若相邻 Chunk 都不空闲，则保留当前 Chunk 并把它移到链表头部，等待复用。
    ///
    /// 该策略保证不会出现“只有一个空 Chunk 且被立即释放、随后插入又立刻分配”的
    /// 抖动，同时让连续产生的空闲 Chunk 有机会归还给底层 allocator。
    ///
    /// # Panics
    ///
    /// 如果 `ptr` 对应的槽位已经被归还（`self_idx` 的 occupied 位为 0），
    /// 本方法会 panic。这样可以在调试和测试中尽早发现重复归还。
    ///
    /// # 句柄有效性
    ///
    /// `ptr` 必须由本 arena 的 `insert` 返回，并且在本 arena 存活期间保持有效。
    /// 传入其他 arena 的句柄、已经悬垂的句柄或伪造的句柄属于调用方逻辑错误，
    /// 可能触发未定义行为。`SlabList` 不会扫描 Chunk 链表来验证句柄归属。
    ///
    /// `allocator` 必须是分配该 Chunk 时使用的同一个 allocator 实例；否则
    /// `deallocate` 的调用方契约无法满足。
    ///
    /// # Examples
    ///
    /// ```text
    /// use crate::slab_list_::{SlabList, SlabListConfig};
    /// use std::alloc::Global;
    ///
    /// let mut arena = SlabList::<u64, SlabListConfig<4096>>::new().unwrap();
    /// let a = arena.insert(1, &Global);
    /// let b = arena.insert(2, &Global);
    ///
    /// assert_eq!(arena.remove(a, &Global), 1);
    /// assert_eq!(*b, 2);
    /// arena.clear(&Global);
    /// ```
    pub fn remove(&mut self, ptr: SlotPtr<T>, allocator: &C::Allocator) -> T {
        let chunk_layout = self.chunk_layout_();

        // SAFETY:
        // - SlotPtr 的安全约定保证 ptr 指向本 arena 中一个已初始化的槽位。
        // - 槽位基址就是 T 的地址，因此可以 cast 回 Slot<T> 并读取元数据。
        // - allocator 由调用方保证与分配 Chunk 时使用的是同一个实例。
        unsafe {
            let slot = ptr.as_non_null_().as_ptr().cast::<Slot<T>>();
            let self_idx = ptr::read(slot_self_idx_ptr_(slot));

            assert!(
                self_idx & OCCUPIED_BIT != 0,
                "remove called with a freed or invalid SlotPtr"
            );

            let idx = self_idx & SLOT_INDEX_MASK;
            let chunk = chunk_of_slot_(slot, idx, chunk_layout.slot_offset_);

            debug_assert!((idx as usize) < (*chunk).capacity_ as usize);
            debug_assert!(self.contains_chunk_(chunk));
            debug_assert_eq!(
                slot_ptr_at_::<T>(chunk, chunk_layout.slot_offset_, idx),
                slot
            );

            // 先把元素移出，再修改槽位元数据；这样返回值不依赖空闲链表状态。
            let value = slot.cast::<T>().read();

            (*slot).self_idx_ = idx;
            (*slot).next_free_ = (*chunk).free_head_;
            (*chunk).free_head_ = idx;
            (*chunk).free_count_ += 1;

            if (*chunk).free_count_ < (*chunk).capacity_ {
                self.move_to_head_(chunk);
            } else {
                // 刚回收的是该 Chunk 的最后一个非空闲槽位：立刻自动尝试归还。
                self.reclaim_empty_chunk_(chunk, chunk_layout, allocator);
            }

            value
        }
    }

    /// 返回链表头 Chunk 的空闲槽数，仅供单元测试观察近似排序策略。
    #[cfg(test)]
    fn head_free_count_(&self) -> u16 {
        if self.head_.is_null() {
            0
        } else {
            // SAFETY: head_ 要么为空，要么指向有效 Chunk。
            unsafe { (*self.head_).free_count_ }
        }
    }

    /// 返回当前 Chunk 数量，仅供单元测试观察链表状态。
    #[cfg(test)]
    fn chunk_count_(&self) -> usize {
        let mut count = 0;
        let mut current = self.head_;
        while !current.is_null() {
            count += 1;
            // SAFETY: current 非空时指向有效 Chunk。
            current = unsafe { (*current).next_ };
        }
        count
    }

    /// 确保链表头部指向一个有空闲槽位的 Chunk，并返回它。
    ///
    /// 根据链表的维护策略，如果头部 Chunk 已满，则所有 Chunk 都已满，
    /// 因此直接分配一个新 Chunk 放到头部即可。
    fn ensure_free_chunk_(
        &mut self,
        chunk_layout: ChunkLayout,
        allocator: &C::Allocator,
    ) -> *mut ChunkHeader {
        let must_alloc = if self.head_.is_null() {
            true
        } else {
            // SAFETY: head_ 非空时指向有效 Chunk。
            unsafe { (*self.head_).free_count_ == 0 }
        };

        if must_alloc {
            self.alloc_chunk_front_(chunk_layout, allocator);
        }

        debug_assert!(!self.head_.is_null());
        // SAFETY: 上面保证 head_ 非空，且 alloc_chunk_front_ 分配的 Chunk
        // 至少有一个空闲槽位。
        self.head_
    }

    /// 使用调用方借入的 allocator 分配一个新 Chunk 并插入链表头部。
    fn alloc_chunk_front_(
        &mut self,
        chunk_layout: ChunkLayout,
        allocator: &C::Allocator,
    ) -> *mut ChunkHeader {
        // SAFETY: chunk_layout 由 C::CHUNK_SIZE 与 T 成功计算，
        // 且 allocator 的类型就是 C::Allocator。
        let chunk = unsafe { alloc_chunk_::<T, _>(allocator, chunk_layout) };

        // SAFETY: chunk 是新分配的有效 Chunk；head_/tail_ 要么为空，
        // 要么指向链表中仍然有效的 Chunk。
        unsafe {
            (*chunk).prev_ = ptr::null_mut();
            (*chunk).next_ = self.head_;

            if self.head_.is_null() {
                self.tail_ = chunk;
            } else {
                (*self.head_).prev_ = chunk;
            }
        }

        self.head_ = chunk;
        if self.tail_.is_null() {
            self.tail_ = chunk;
        }

        chunk
    }

    /// 把一个 Chunk 移到链表头部。
    ///
    /// 调用者必须保证 `chunk` 属于本链表。
    fn move_to_head_(&mut self, chunk: *mut ChunkHeader) {
        if chunk == self.head_ {
            return;
        }

        // SAFETY: chunk 是链表中的有效节点；prev_/next_ 构成一致的双向链表。
        unsafe {
            let prev = (*chunk).prev_;
            let next = (*chunk).next_;

            debug_assert!(!prev.is_null(), "chunk list is corrupted");
            if !prev.is_null() {
                (*prev).next_ = next;
            }

            if next.is_null() {
                self.tail_ = prev;
            } else {
                (*next).prev_ = prev;
            }

            (*chunk).prev_ = ptr::null_mut();
            (*chunk).next_ = self.head_;

            if self.head_.is_null() {
                self.tail_ = chunk;
            } else {
                (*self.head_).prev_ = chunk;
            }

            self.head_ = chunk;
        }
    }

    /// 在 Chunk 刚变为完全空闲时自动尝试归还它的内存。
    ///
    /// 该函数只由 [`SlabList::remove`] 在 `free_count_ == capacity_` 的那个调用
    /// 中直接执行，不依赖任何额外的 release/shrink 方法。
    fn reclaim_empty_chunk_(
        &mut self,
        chunk: *mut ChunkHeader,
        chunk_layout: ChunkLayout,
        allocator: &C::Allocator,
    ) {
        // 唯一 Chunk 必须保留，否则下一次插入会立即重新分配。
        if self.head_ == self.tail_ {
            return;
        }

        // SAFETY: chunk 是当前链表中有效的 Chunk，来自 remove 中刚归还完槽位的
        // Chunk；prev_/next_ 与 free_count_/capacity_ 均由链表维护。
        unsafe {
            let prev = (*chunk).prev_;
            let next = (*chunk).next_;
            let prev_free = !prev.is_null() && (*prev).free_count_ == (*prev).capacity_;
            let next_free = !next.is_null() && (*next).free_count_ == (*next).capacity_;

            if prev_free || next_free {
                self.unlink_chunk_(chunk);
                dealloc_chunk_(allocator, chunk, chunk_layout.layout_);
            } else {
                self.move_to_head_(chunk);
            }
        }
    }

    /// 把 `chunk` 从 Chunk 链表中摘下，但不释放它的内存。
    ///
    /// 调用者必须保证 `chunk` 属于本链表，且之后要么把它重新挂回链表，要么
    /// 通过 allocator 释放。
    fn unlink_chunk_(&mut self, chunk: *mut ChunkHeader) {
        // SAFETY: 调用者保证 chunk 是链表中有效的节点，prev_/next_ 一致。
        unsafe {
            let prev = (*chunk).prev_;
            let next = (*chunk).next_;

            if prev.is_null() {
                self.head_ = next;
            } else {
                (*prev).next_ = next;
            }

            if next.is_null() {
                self.tail_ = prev;
            } else {
                (*next).prev_ = prev;
            }

            (*chunk).prev_ = ptr::null_mut();
            (*chunk).next_ = ptr::null_mut();
        }
    }

    /// 把一个 Chunk 移到链表尾部。
    ///
    /// 调用者必须保证 `chunk` 属于本链表。
    fn move_to_tail_(&mut self, chunk: *mut ChunkHeader) {
        if chunk == self.tail_ {
            return;
        }

        // SAFETY: chunk 是链表中的有效节点；prev_/next_ 构成一致的双向链表。
        unsafe {
            let prev = (*chunk).prev_;
            let next = (*chunk).next_;

            if prev.is_null() {
                self.head_ = next;
            } else {
                (*prev).next_ = next;
            }

            debug_assert!(!next.is_null(), "chunk list is corrupted");
            if next.is_null() {
                self.tail_ = prev;
            } else {
                (*next).prev_ = prev;
            }

            (*chunk).prev_ = self.tail_;
            (*chunk).next_ = ptr::null_mut();

            if self.tail_.is_null() {
                self.head_ = chunk;
            } else {
                (*self.tail_).next_ = chunk;
            }

            self.tail_ = chunk;
        }
    }

    /// 判断 `target` 是否在当前 Chunk 链表中。
    ///
    /// 该函数只用于 `debug_assert!` 的归属校验，不会进入 release 热路径。
    fn contains_chunk_(&self, target: *mut ChunkHeader) -> bool {
        let mut current = self.head_;
        while !current.is_null() {
            if current == target {
                return true;
            }
            // SAFETY: current 非空时指向有效 Chunk。
            current = unsafe { (*current).next_ };
        }
        false
    }
}

impl<T> SlabList<T, SlabListConfig<DEFAULT_CHUNK_BYTES>> {
    /// 使用默认配置创建一个空的 `SlabList`。
    ///
    /// 默认配置使用 64 KiB 的单 Chunk 大小和 [`Global`] allocator。
    ///
    /// # Errors
    ///
    /// 与 [`SlabList::new`] 相同。
    pub fn with_default_config() -> Result<Self, SlabListError> {
        SlabList::new()
    }
}

impl<T, C> SlabList<T, C>
where
    C: TrConfig,
{
    /// 析构全部存活元素、释放所有 Chunk，并恢复为空 arena。
    ///
    /// `allocator` 必须与分配这些 Chunk 时使用的是同一个 allocator（或能
    /// 正确释放同一布局的等价 allocator），否则属于调用方逻辑错误。
    ///
    /// 释放前会先对仍处于 occupied 状态的槽位执行 `drop_in_place`；已经归还
    /// 的空闲槽位不会被重复析构。
    ///
    /// # Examples
    ///
    /// ```text
    /// use alloc::alloc::Global;
    /// use crate::slab_list_::{SlabList, SlabListConfig};
    ///
    /// let mut arena = SlabList::<u64, SlabListConfig<4096>>::new().unwrap();
    /// let a = arena.insert(1, &Global);
    /// let _b = arena.insert(2, &Global);
    /// arena.remove(a, &Global);
    /// arena.clear(&Global);
    /// ```
    pub fn clear(&mut self, allocator: &C::Allocator) {
        let chunk_layout = self.chunk_layout_();
        let mut chunk = self.head_;

        while !chunk.is_null() {
            // SAFETY:
            // - chunk 是当前链表中尚未释放的节点。
            // - 每个槽位的元数据在分配 Chunk 时已经初始化，之后只会被读写而不会失效。
            // - capacity_ 与槽位数组的实际长度一致。
            // - 只有 occupied 位为 1 的槽位包含已初始化的 T，需要且只需要析构一次。
            unsafe {
                let next = (*chunk).next_;
                let capacity = (*chunk).capacity_ as usize;
                let slots = slots_base_::<T>(chunk, chunk_layout.slot_offset_);

                for i in 0..capacity {
                    let slot = slots.add(i);
                    let self_idx = ptr::read(slot_self_idx_ptr_(slot));
                    if self_idx & OCCUPIED_BIT != 0 {
                        ptr::drop_in_place(slot.cast::<T>());
                    }
                }

                dealloc_chunk_(allocator, chunk, chunk_layout.layout_);
                chunk = next;
            }
        }

        self.head_ = ptr::null_mut();
        self.tail_ = ptr::null_mut();
    }
}

impl<T, C> Drop for SlabList<T, C>
where
    C: TrConfig,
{
    fn drop(&mut self) {
        // SlabList 不持有 allocator，无法在 Drop 中释放 Chunk；调用方必须先调用
        // clear。树类型在自身的 Drop 中完成这一步。
        debug_assert!(
            self.head_.is_null(),
            "SlabList 必须在释放前调用 clear，否则会泄漏 Chunk"
        );
    }
}

#[cfg(test)]
mod tests {
    use alloc::alloc::Global;
    use core::{
        alloc::{AllocError, Allocator, Layout},
        cell::Cell,
        mem::size_of,
        ptr::NonNull,
    };
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        rc::Rc,
    };

    use super::super::{
        chunk_::{Slot, slot_offset_},
        config_::MAX_SLOTS_PER_CHUNK,
    };
    use super::*;

    /// 一个把分配/释放次数记录在 `Rc<Cell<_>>` 中的 allocator。
    struct CountingAlloc {
        allocs_: Rc<Cell<usize>>,
        deallocs_: Rc<Cell<usize>>,
    }

    // SAFETY: CountingAlloc 将所有分配/释放委托给 Global，并只额外记录次数；
    // 因此它满足 Allocator 对指针有效性、布局匹配和分配失败的要求。
    unsafe impl Allocator for CountingAlloc {
        fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            let allocation = Global.allocate(layout)?;
            self.allocs_.set(self.allocs_.get() + 1);
            Ok(allocation)
        }

        unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
            // SAFETY: 调用者保证 ptr/layout 来自本 allocator 的 allocate；
            // 本实现将分配委托给 Global。
            unsafe { Global.deallocate(ptr, layout) };
            self.deallocs_.set(self.deallocs_.get() + 1);
        }
    }

    /// 计算刚好容纳 `slots` 个槽位的 Chunk 字节数。
    const fn chunk_bytes_for_slots_<T>(slots: usize) -> usize {
        let offset = match slot_offset_::<T>() {
            Some(offset) => offset,
            None => panic!("Chunk header 的偏移应当可以计算"),
        };
        offset + size_of::<Slot<T>>() * slots
    }

    /// 每 Chunk 4 个 `u32` 槽位的配置类型。
    type U32Cfg4 = SlabListConfig<{ chunk_bytes_for_slots_::<u32>(4) }>;

    /// 每 Chunk 2 个 `u32` 槽位的配置类型。
    type U32Cfg2 = SlabListConfig<{ chunk_bytes_for_slots_::<u32>(2) }>;

    /// 每 Chunk 4 个 ZST 槽位的配置类型。
    type ZstCfg4 = SlabListConfig<{ chunk_bytes_for_slots_::<()>(4) }>;

    /// 使用 `CountingAlloc` 的每 Chunk 2 个 `u32` 槽位配置。
    type CountingCfg2 = SlabListConfig<{ chunk_bytes_for_slots_::<u32>(2) }, CountingAlloc>;

    /// 使用 `CountingAlloc` 的每 Chunk 1 个 `u32` 槽位配置。
    type CountingCfg1 = SlabListConfig<{ chunk_bytes_for_slots_::<u32>(1) }, CountingAlloc>;

    /// 测试目标：验证 Chunk 字节数不足时拒绝创建 SlabList。
    /// Test goal: Verify that a chunk configuration too small for one slot is rejected.
    ///
    /// 测试手段：分别用 `CHUNK_SIZE = 0` 和不足 `header + 一个槽位` 的 const
    /// 配置构造 `SlabList<u32>`。
    /// Test method: Build `SlabList<u32>` with a `CHUNK_SIZE = 0` config and with a
    /// const config smaller than `header + one slot`.
    ///
    /// 判定标准：两次都返回 `SlabListError::ChunkTooSmall`。
    /// Assertions: Both calls return `SlabListError::ChunkTooSmall`.
    #[test]
    fn test_new_rejects_too_small_chunk() {
        assert_eq!(
            SlabList::<u32, SlabListConfig<0>>::new().err(),
            Some(SlabListError::ChunkTooSmall)
        );

        const TOO_SMALL: usize = chunk_bytes_for_slots_::<u32>(1) - 1;

        assert_eq!(
            SlabList::<u32, SlabListConfig<TOO_SMALL>>::new().err(),
            Some(SlabListError::ChunkTooSmall)
        );
    }

    /// 测试目标：验证槽位数超过 2^15 时拒绝创建 SlabList。
    /// Test goal: Verify that a chunk configuration implying more than 2^15 slots is rejected.
    ///
    /// 测试手段：为 `u8` 构造一个刚好能容纳
    /// `MAX_SLOTS_PER_CHUNK + 1` 个槽位的 const 配置。
    /// Test method: For `u8`, use a const config that fits exactly
    /// `MAX_SLOTS_PER_CHUNK + 1` slots.
    ///
    /// 判定标准：创建返回 `SlabListError::TooManySlots`。
    /// Assertions: Creation returns `SlabListError::TooManySlots`.
    #[test]
    fn test_new_rejects_too_many_slots() {
        const TOO_MANY: usize = chunk_bytes_for_slots_::<u8>(MAX_SLOTS_PER_CHUNK + 1);

        assert_eq!(
            SlabList::<u8, SlabListConfig<TOO_MANY>>::new().err(),
            Some(SlabListError::TooManySlots)
        );
    }

    /// 测试目标：验证基本插入、Deref/DerefMut 以及乱序归还。
    /// Test goal: Verify basic insertion, Deref/DerefMut, and out-of-order removal.
    ///
    /// 测试手段：在每 Chunk 4 个槽位的 arena 中插入 3 个 `u32`，通过句柄修改其中一个，
    /// 然后乱序移除并重新插入，最后调用 `clear` 释放 Chunk。
    /// Test method: Insert three `u32` values into an arena with four slots per chunk,
    /// mutate one through its handle, remove out of order, insert again, and finally
    /// release the chunks with `clear`.
    ///
    /// 判定标准：句柄读写得到预期值；乱序移除返回正确的原值；重新插入后所有槽位
    /// 仍可正常使用。
    /// Assertions: Handles read and write expected values; out-of-order removals return
    /// the correct original values; all slots remain usable after reinsertion.
    #[test]
    fn test_insert_deref_and_remove_out_of_order() {
        let mut arena = SlabList::<u32, U32Cfg4>::new().expect("配置应当有效");

        assert_eq!(arena.slots_per_chunk(), 4);

        let mut a = arena.insert(10, &Global);
        let b = arena.insert(20, &Global);
        let c = arena.insert(30, &Global);

        assert_eq!(*a, 10);
        assert_eq!(*b, 20);
        assert_eq!(*c, 30);

        *a += 5;
        assert_eq!(*a, 15);

        assert_eq!(arena.remove(b, &Global), 20);

        let d = arena.insert(40, &Global);
        assert_eq!(*d, 40);

        assert_eq!(arena.remove(a, &Global), 15);
        assert_eq!(arena.remove(c, &Global), 30);
        assert_eq!(arena.remove(d, &Global), 40);

        let e = arena.insert(50, &Global);
        assert_eq!(*e, 50);

        arena.clear(&Global);
    }

    /// 测试目标：验证归还槽位后优先复用最近释放的槽位。
    /// Test goal: Verify that the most recently freed slot is reused first.
    ///
    /// 测试手段：插入两个元素，先释放第一个再插入第三个，随后释放第二个再插入第四个。
    /// Test method: Insert two elements, free the first before inserting a third, then
    /// free the second before inserting a fourth.
    ///
    /// 判定标准：新插入元素的句柄地址分别等于刚释放槽位的句柄地址。
    /// Assertions: The new handles have the same addresses as the just-freed handles.
    #[test]
    fn test_free_list_reuses_most_recent_slot() {
        let mut arena = SlabList::<u32, U32Cfg4>::new().expect("配置应当有效");

        let a = arena.insert(1, &Global);
        let b = arena.insert(2, &Global);

        assert_eq!(arena.remove(a, &Global), 1);
        let c = arena.insert(3, &Global);
        assert_eq!(a.as_ptr(), c.as_ptr());

        assert_eq!(arena.remove(b, &Global), 2);
        let d = arena.insert(4, &Global);
        assert_eq!(b.as_ptr(), d.as_ptr());

        arena.clear(&Global);
    }

    /// 测试目标：验证 Chunk 满后扩容，以及归还槽位后 Chunk 移回头部并被优先复用。
    /// Test goal: Verify chunk growth and that a chunk is moved to the head and reused
    /// after a slot is freed.
    ///
    /// 测试手段：使用每 Chunk 2 个槽位的配置插入 5 个元素，然后释放第一个元素，
    /// 再插入第 6 个元素；随后释放已经位于链表尾部的第 6 个元素，再插入第 7 个元素，
    /// 最后调用 `clear` 释放 Chunk。
    /// Test method: Use two slots per chunk, insert five elements, free the first one,
    /// then insert a sixth element; after that, free the sixth element (now in the tail
    /// chunk) and insert a seventh element; finally release the chunks with `clear`.
    ///
    /// 判定标准：插入 5 个元素后 Chunk 数为 3、槽位总数为 6（3 × 2）；释放后头部
    /// Chunk 有 1 个空闲槽；再次插入复用原槽位且 Chunk 数、槽位总数都不增加；
    /// 从尾部释放后同样复用原槽位。
    /// Assertions: After five inserts there are three chunks and six slots in total
    /// (3 × 2); after freeing, the head chunk has one free slot; the next insert reuses
    /// that slot without adding a chunk or slot; freeing from the tail chunk reuses its
    /// slot as well.
    #[test]
    fn test_chunk_growth_and_move_to_head() {
        let mut arena = SlabList::<u32, U32Cfg2>::new().expect("配置应当有效");

        let a = arena.insert(0, &Global);
        let _b = arena.insert(1, &Global);
        let _c = arena.insert(2, &Global);
        let _d = arena.insert(3, &Global);
        let _e = arena.insert(4, &Global);

        assert_eq!(arena.slots_per_chunk(), 2);
        assert_eq!(arena.chunk_count_(), 3);
        assert_eq!(arena.chunk_count_() * arena.slots_per_chunk(), 6);

        assert_eq!(arena.remove(a, &Global), 0);
        assert_eq!(arena.chunk_count_(), 3);
        assert_eq!(arena.head_free_count_(), 1);

        let f = arena.insert(5, &Global);
        assert_eq!(arena.chunk_count_(), 3);
        assert_eq!(arena.chunk_count_() * arena.slots_per_chunk(), 6);
        assert_eq!(f.as_ptr(), a.as_ptr());

        // f 所在的 Chunk 刚刚被移到尾部；再次释放它时，该 Chunk 应移回头部。
        // The chunk holding f was just moved to the tail; freeing it should move the
        // chunk back to the head.
        assert_eq!(arena.remove(f, &Global), 5);
        assert_eq!(arena.head_free_count_(), 1);

        let g = arena.insert(6, &Global);
        assert_eq!(g.as_ptr(), f.as_ptr());
        assert_eq!(arena.chunk_count_(), 3);
        assert_eq!(arena.chunk_count_() * arena.slots_per_chunk(), 6);

        arena.clear(&Global);
    }

    /// 测试目标：验证完全空闲的 Chunk 在前驱也空闲时归还给 allocator。
    /// Test goal: Verify that a fully free chunk is returned to the allocator when its
    /// previous neighbor is also fully free.
    ///
    /// 测试手段：使用每 Chunk 1 个槽位的 `CountingAlloc` arena 插入 3 个元素，
    /// 先释放中间元素（相邻两个 Chunk 都非空，因此保留），再释放第一个元素，
    /// 使它的前驱是刚释放的空 Chunk；最后释放最后一个元素。
    /// Test method: Insert three elements into a one-slot-per-chunk arena backed by
    /// `CountingAlloc`; free the middle element first (both neighbors are non-free, so it
    /// stays), then free the first element so its previous neighbor is the free chunk,
    /// and finally free the last element.
    ///
    /// 判定标准：释放中间元素时没有 deallocate；释放第一个元素后释放次数为 1、
    /// Chunk 数为 2；释放最后一个元素后释放次数为 2、Chunk 数为 1；`clear` 后
    /// 总释放次数与总分配次数相等。
    /// Assertions: No deallocation after freeing the middle element; one deallocation
    /// and two chunks after the first; two deallocations and one chunk after the last;
    /// after `clear`, total deallocations equal total allocations.
    #[test]
    fn test_reclaim_chunk_with_free_previous_neighbor() {
        let allocs = Rc::new(Cell::new(0));
        let deallocs = Rc::new(Cell::new(0));
        let allocator = CountingAlloc {
            allocs_: Rc::clone(&allocs),
            deallocs_: Rc::clone(&deallocs),
        };

        let mut arena = SlabList::<u32, CountingCfg1>::new().expect("配置应当有效");

        let a = arena.insert(1, &allocator);
        let b = arena.insert(2, &allocator);
        let c = arena.insert(3, &allocator);

        assert_eq!(allocs.get(), 3);
        assert_eq!(arena.chunk_count_(), 3);

        // 中间 Chunk 变为完全空闲，但前后邻居都非空：保留并移到头部。
        assert_eq!(arena.remove(b, &allocator), 2);
        assert_eq!(deallocs.get(), 0);
        assert_eq!(arena.chunk_count_(), 3);

        // 第一个 Chunk 成为完全空闲，其前驱是刚释放的空 Chunk：归还自身。
        assert_eq!(arena.remove(a, &allocator), 1);
        assert_eq!(deallocs.get(), 1);
        assert_eq!(arena.chunk_count_(), 2);

        // 最后一个 Chunk 的前驱仍是保留的空 Chunk：继续归还。
        assert_eq!(arena.remove(c, &allocator), 3);
        assert_eq!(deallocs.get(), 2);
        assert_eq!(arena.chunk_count_(), 1);

        arena.clear(&allocator);
        assert_eq!(deallocs.get(), 3);
        assert_eq!(allocs.get(), 3);
    }

    /// 测试目标：验证完全空闲的 Chunk 在后继也空闲时归还给 allocator。
    /// Test goal: Verify that a fully free chunk is returned to the allocator when its
    /// next neighbor is also fully free.
    ///
    /// 测试手段：使用每 Chunk 2 个槽位的 `CountingAlloc` arena 填满两个 Chunk，
    /// 先清空第一个 Chunk（后继仍非空，保留），再清空第二个 Chunk（后继为第一个
    /// 空 Chunk）；随后重新插入并再次释放。
    /// Test method: Fill two chunks in a two-slot-per-chunk arena backed by
    /// `CountingAlloc`; clear the first chunk (its next neighbor still has live
    /// elements, so it stays), then clear the second chunk (its next neighbor is the
    /// free first chunk); then insert and free again.
    ///
    /// 判定标准：清空第一个 Chunk 后没有释放；清空第二个后释放 1 个 Chunk，
    /// 且剩余 Chunk 仍可复用；`clear` 后释放次数等于分配次数。
    /// Assertions: No deallocation after clearing the first chunk; one chunk is
    /// deallocated after clearing the second; the remaining chunk is reusable; after
    /// `clear`, deallocations equal allocations.
    #[test]
    fn test_reclaim_chunk_with_free_next_neighbor() {
        let allocs = Rc::new(Cell::new(0));
        let deallocs = Rc::new(Cell::new(0));
        let allocator = CountingAlloc {
            allocs_: Rc::clone(&allocs),
            deallocs_: Rc::clone(&deallocs),
        };

        let mut arena = SlabList::<u32, CountingCfg2>::new().expect("配置应当有效");

        let a0 = arena.insert(0, &allocator);
        let a1 = arena.insert(1, &allocator);
        let b0 = arena.insert(2, &allocator);
        let b1 = arena.insert(3, &allocator);

        assert_eq!(allocs.get(), 2);
        assert_eq!(arena.chunk_count_(), 2);

        // 第一个 Chunk 完全空闲，但后继 Chunk 仍有存活元素：保留。
        assert_eq!(arena.remove(a0, &allocator), 0);
        assert_eq!(arena.remove(a1, &allocator), 1);
        assert_eq!(deallocs.get(), 0);
        assert_eq!(arena.chunk_count_(), 2);

        // 第二个 Chunk 变空，其后继是第一个空 Chunk：归还第二个 Chunk。
        assert_eq!(arena.remove(b0, &allocator), 2);
        assert_eq!(arena.remove(b1, &allocator), 3);
        assert_eq!(deallocs.get(), 1);
        assert_eq!(arena.chunk_count_(), 1);

        // 剩下的空 Chunk 应继续复用，而不是重新分配。
        let c = arena.insert(4, &allocator);
        assert_eq!(*c, 4);
        assert_eq!(allocs.get(), 2);
        assert_eq!(arena.remove(c, &allocator), 4);
        assert_eq!(deallocs.get(), 1);

        arena.clear(&allocator);
        assert_eq!(deallocs.get(), 2);
        assert_eq!(allocs.get(), 2);
    }

    /// 测试目标：验证最后一个非空闲槽位被回收时，Chunk 在同一次 `remove` 调用中
    /// 自动归还，不需要任何额外的 release/shrink 方法。
    /// Test goal: Verify that when the last non-free slot is reclaimed, the chunk is
    /// returned automatically within the same `remove` call, without any separate
    /// release/shrink method.
    ///
    /// 测试手段：使用每 Chunk 2 个槽位的 `CountingAlloc` arena 填满两个 Chunk，
    /// 先清空第一个 Chunk（后继非空，保留），再清空第二个 Chunk；第二次清空的最后
    /// 一次 `remove` 返回后立即检查释放计数。
    /// Test method: Fill two chunks in a two-slot-per-chunk arena backed by
    /// `CountingAlloc`; clear the first chunk (its next neighbor is non-free, so it
    /// stays), then clear the second chunk and check the deallocation count immediately
    /// after the final `remove` returns.
    ///
    /// 判定标准：清空第一个 Chunk 时释放次数为 0；最后一个槽位归还后，无需调用
    /// 其他方法，释放次数立即变为 1，Chunk 数立即变为 1。
    /// Assertions: No deallocation after clearing the first chunk; immediately after the
    /// last slot is returned, the deallocation count becomes 1 and the chunk count
    /// becomes 1 without calling any other method.
    #[test]
    fn test_last_slot_removal_automatically_reclaims_chunk() {
        let allocs = Rc::new(Cell::new(0));
        let deallocs = Rc::new(Cell::new(0));
        let allocator = CountingAlloc {
            allocs_: Rc::clone(&allocs),
            deallocs_: Rc::clone(&deallocs),
        };

        let mut arena = SlabList::<u32, CountingCfg2>::new().expect("配置应当有效");

        let a0 = arena.insert(0, &allocator);
        let a1 = arena.insert(1, &allocator);
        let b0 = arena.insert(2, &allocator);
        let b1 = arena.insert(3, &allocator);

        assert_eq!(allocs.get(), 2);
        assert_eq!(arena.chunk_count_(), 2);

        assert_eq!(arena.remove(a0, &allocator), 0);
        assert_eq!(arena.remove(a1, &allocator), 1);
        assert_eq!(deallocs.get(), 0);
        assert_eq!(arena.chunk_count_(), 2);

        assert_eq!(arena.remove(b0, &allocator), 2);
        assert_eq!(deallocs.get(), 0);

        // 这是第二个 Chunk 的最后一个非空闲槽位；remove 返回时应当已经完成归还。
        assert_eq!(arena.remove(b1, &allocator), 3);
        assert_eq!(deallocs.get(), 1);
        assert_eq!(arena.chunk_count_(), 1);

        arena.clear(&allocator);
        assert_eq!(deallocs.get(), 2);
        assert_eq!(allocs.get(), 2);
    }

    /// 测试目标：验证唯一 Chunk 完全空闲时不会被释放。
    /// Test goal: Verify that the only chunk is retained when it becomes fully free.
    ///
    /// 测试手段：使用每 Chunk 2 个槽位的 `CountingAlloc` arena，插入并释放全部元素；
    /// 随后再次插入一个元素并释放，最后调用 `clear`。
    /// Test method: Use a two-slot-per-chunk arena backed by `CountingAlloc`, insert
    /// and free all elements, then insert and free one more, and finally call `clear`.
    ///
    /// 判定标准：前两轮释放都没有触发 deallocate；再次插入不增加分配次数；只有
    /// `clear` 才释放这个唯一 Chunk。
    /// Assertions: The first two removals do not deallocate; the next insert does not
    /// allocate; only `clear` releases the only chunk.
    #[test]
    fn test_keep_only_free_chunk() {
        let allocs = Rc::new(Cell::new(0));
        let deallocs = Rc::new(Cell::new(0));
        let allocator = CountingAlloc {
            allocs_: Rc::clone(&allocs),
            deallocs_: Rc::clone(&deallocs),
        };

        let mut arena = SlabList::<u32, CountingCfg2>::new().expect("配置应当有效");

        let a = arena.insert(1, &allocator);
        let b = arena.insert(2, &allocator);
        assert_eq!(allocs.get(), 1);

        assert_eq!(arena.remove(a, &allocator), 1);
        assert_eq!(arena.remove(b, &allocator), 2);
        assert_eq!(deallocs.get(), 0);
        assert_eq!(arena.chunk_count_(), 1);

        let c = arena.insert(3, &allocator);
        assert_eq!(*c, 3);
        assert_eq!(allocs.get(), 1);
        assert_eq!(arena.remove(c, &allocator), 3);
        assert_eq!(deallocs.get(), 0);
        assert_eq!(arena.chunk_count_(), 1);

        arena.clear(&allocator);
        assert_eq!(deallocs.get(), 1);
        assert_eq!(allocs.get(), 1);
    }

    /// 测试目标：验证 `clear` 只析构仍存活的元素，不会重复析构已归还槽位。
    /// Test goal: Verify that `clear` drops only live elements and never double-drops a
    /// freed slot.
    ///
    /// 测试手段：用带析构计数器的 `DropProbe` 在每 Chunk 2 个槽位的 arena 中插入
    /// 3 个元素（跨越两个 Chunk），移除其中一个并立即析构，然后调用 `clear`。
    /// Test method: Insert three `DropProbe` values across two chunks (two slots per
    /// chunk), remove one and drop it immediately, then call `clear`.
    ///
    /// 判定标准：移除后计数器为 1；`clear` 后计数器为 3，说明只有两个存活元素
    /// 被 arena 析构，空闲槽位没有被重复析构。
    /// Assertions: The counter is 1 after the explicit removal and 3 after `clear`,
    /// meaning only the two live elements were dropped by the arena.
    #[test]
    fn test_clear_drops_only_live_elements() {
        struct DropProbe {
            counter_: Rc<Cell<usize>>,
            value_: u32,
        }

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.counter_.set(self.counter_.get() + 1);
            }
        }

        type DropCfg2 = SlabListConfig<{ chunk_bytes_for_slots_::<DropProbe>(2) }>;

        let counter = Rc::new(Cell::new(0));
        let mut arena = SlabList::<DropProbe, DropCfg2>::new().expect("配置应当有效");

        let a = arena.insert(
            DropProbe {
                counter_: Rc::clone(&counter),
                value_: 1,
            },
            &Global,
        );
        let _b = arena.insert(
            DropProbe {
                counter_: Rc::clone(&counter),
                value_: 2,
            },
            &Global,
        );
        let _c = arena.insert(
            DropProbe {
                counter_: Rc::clone(&counter),
                value_: 3,
            },
            &Global,
        );

        let removed = arena.remove(a, &Global);
        assert_eq!(removed.value_, 1);
        drop(removed);
        assert_eq!(counter.get(), 1);

        arena.clear(&Global);
        assert_eq!(counter.get(), 3);
    }

    /// 测试目标：验证零大小类型也能获得互相区分且可乱序归还的槽位。
    /// Test goal: Verify that zero-sized types get distinct, individually reclaimable slots.
    ///
    /// 测试手段：在可容纳 4 个槽位的 `SlabList<()>` 中插入 3 个 `()`，移除中间一个，
    /// 再插入一个，最后调用 `clear`。
    /// Test method: Insert three `()` values into a four-slot `SlabList<()>`, remove the
    /// middle one, insert another, and call `clear`.
    ///
    /// 判定标准：三个初始句柄地址互不相同；重新插入复用被释放槽位的地址。
    /// Assertions: The three initial handles have distinct addresses; the new insert
    /// reuses the freed slot address.
    #[test]
    fn test_zero_sized_type_slots() {
        let mut arena = SlabList::<(), ZstCfg4>::new().expect("配置应当有效");

        let a = arena.insert((), &Global);
        let b = arena.insert((), &Global);
        let c = arena.insert((), &Global);

        assert_ne!(a.as_ptr(), b.as_ptr());
        assert_ne!(b.as_ptr(), c.as_ptr());

        assert_eq!(arena.remove(b, &Global), ());
        let d = arena.insert((), &Global);

        assert_eq!(d.as_ptr(), b.as_ptr());

        arena.clear(&Global);
    }

    /// 测试目标：验证对已归还槽位再次 `remove` 会 panic。
    /// Test goal: Verify that removing the same slot twice panics.
    ///
    /// 测试手段：插入一个元素，归还一次，然后用同一个句柄再次归还；用
    /// `catch_unwind` 捕获 panic 后调用 `clear` 释放 Chunk。
    /// Test method: Insert one element, remove it once, then remove it again with the
    /// same handle; catch the panic with `catch_unwind` and call `clear` afterwards.
    ///
    /// 判定标准：第二次调用触发 panic，`catch_unwind` 返回 `Err`。
    /// Assertions: The second call panics, so `catch_unwind` returns `Err`.
    #[test]
    fn test_double_remove_panics() {
        let mut arena = SlabList::<u32, U32Cfg2>::new().expect("配置应当有效");

        let a = arena.insert(1, &Global);
        assert_eq!(arena.remove(a, &Global), 1);

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = arena.remove(a, &Global);
        }));

        assert!(result.is_err());
        arena.clear(&Global);
    }

    /// 测试目标：验证默认配置可以创建 arena，且初始状态正确。
    /// Test goal: Verify that the default configuration creates a valid arena with the
    /// expected initial state.
    ///
    /// 测试手段：使用 `with_default_config` 创建 `SlabList<u32>`。
    /// Test method: Create `SlabList<u32>` through `with_default_config`.
    ///
    /// 判定标准：Chunk 字节数与默认常量一致；每 Chunk 槽位数大于 0；尚未分配 Chunk。
    /// Assertions: Chunk bytes match the default constant; slots per chunk are positive;
    /// no chunk is allocated yet.
    #[test]
    fn test_default_config() {
        let arena = SlabList::<u32>::with_default_config().expect("默认配置应当有效");

        assert_eq!(arena.chunk_bytes(), DEFAULT_CHUNK_BYTES);
        assert!(arena.slots_per_chunk() > 0);
        assert_eq!(arena.chunk_count_(), 0);
    }

    /// 测试目标：验证自定义 `TrConfig` 实现可以作为 `SlabList` 的配置参数。
    /// Test goal: Verify that a custom `TrConfig` implementation can be used as the
    /// configuration parameter of `SlabList`.
    ///
    /// 测试手段：定义一个只指定 allocator 类型和 `CHUNK_SIZE` 的自定义配置类型，
    /// 用 `SlabList<u32, TestConfig>` 创建 arena 并插入 3 个元素，最后调用 `clear`。
    /// Test method: Define a custom config that only specifies the allocator type and
    /// `CHUNK_SIZE`, create `SlabList<u32, TestConfig>`, insert three elements, and call
    /// `clear`.
    ///
    /// 判定标准：元素可正常读写，`slots_per_chunk` 来自自定义配置的 `CHUNK_SIZE`。
    /// Assertions: Elements can be read and written normally, and `slots_per_chunk`
    /// reflects the custom config's `CHUNK_SIZE`.
    #[test]
    fn test_custom_config_is_used() {
        struct TestConfig;

        impl TrConfig for TestConfig {
            type Allocator = Global;

            const CHUNK_SIZE: usize = chunk_bytes_for_slots_::<u32>(2);
        }

        let mut arena = SlabList::<u32, TestConfig>::new().expect("配置应当有效");

        assert_eq!(arena.chunk_bytes(), chunk_bytes_for_slots_::<u32>(2));
        assert_eq!(arena.slots_per_chunk(), 2);

        let a = arena.insert(1, &Global);
        let b = arena.insert(2, &Global);
        let c = arena.insert(3, &Global);

        assert_eq!(*a, 1);
        assert_eq!(*b, 2);
        assert_eq!(*c, 3);

        arena.clear(&Global);
    }

    /// 测试目标：验证外部 `Allocator` 被用于 Chunk 的分配与释放。
    /// Test goal: Verify that an external `Allocator` is used for chunk allocation and
    /// deallocation.
    ///
    /// 测试手段：用记录分配/释放次数的 `CountingAlloc` 创建每 Chunk 2 个槽位的 arena，
    /// 插入 3 个元素触发两次 Chunk 分配，然后调用 `clear`。
    /// Test method: Create a two-slot-per-chunk arena with a `CountingAlloc` that records
    /// allocation/deallocation counts, insert three elements to trigger two chunk
    /// allocations, then call `clear`.
    ///
    /// 判定标准：创建后分配次数为 0；第一次插入后为 1；第三次插入后为 2；
    /// `clear` 后释放次数为 2。
    /// Assertions: No allocation occurs on construction; one after the first insert; two
    /// after the third insert; and two deallocations after `clear`.
    #[test]
    fn test_external_allocator_is_used() {
        let allocs = Rc::new(Cell::new(0));
        let deallocs = Rc::new(Cell::new(0));
        let allocator = CountingAlloc {
            allocs_: Rc::clone(&allocs),
            deallocs_: Rc::clone(&deallocs),
        };

        let mut arena = SlabList::<u32, CountingCfg2>::new().expect("配置应当有效");

        assert_eq!(allocs.get(), 0);

        let a = arena.insert(1, &allocator);
        assert_eq!(*a, 1);
        assert_eq!(allocs.get(), 1);

        let _b = arena.insert(2, &allocator);
        assert_eq!(allocs.get(), 1);

        let _c = arena.insert(3, &allocator);
        assert_eq!(allocs.get(), 2);
        assert_eq!(allocator.allocs_.get(), 2);

        assert_eq!(deallocs.get(), 0);

        arena.clear(&allocator);
        assert_eq!(deallocs.get(), 2);
    }
}
