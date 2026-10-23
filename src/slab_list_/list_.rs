//! 基于 Chunk 链表的 SlabList arena。

use core::{
    alloc::Allocator,
    marker::PhantomData,
    ptr::{self, NonNull},
};

use alloc::alloc::Global;

use super::{
    chunk_::{
        ChunkHeader, ChunkLayout, NIL_SLOT, OCCUPIED_BIT, SLOT_INDEX_MASK, Slot, alloc_chunk_,
        chunk_of_slot_, dealloc_chunk_, slot_ptr_at_, slot_self_idx_ptr_, slots_base_,
    },
    config_::SlabListConfig,
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
/// - 某个 Chunk 归还槽位后，会被移到链表头部，以便后续插入优先复用它。
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
/// Chunk 内存由类型参数 `A` 指定的 [`Allocator`] 分配；默认使用 [`Global`]。
/// 单 Chunk 的布局、槽位偏移和槽位数不作为独立字段缓存在 `SlabList` 中，
/// 而是由 [`SlabListConfig`] 结合 `T` 在需要时计算。
///
/// `SlabList` 不保存存活元素数量、总容量或 Chunk 数量等使用统计信息；
/// 它只负责管理 Chunk 链表和槽位的分配/归还。
///
/// # Examples
///
/// ```
/// use bplustree_cmp::{SlabList, SlabListConfig};
///
/// let config = SlabListConfig::new(4096);
/// let mut arena = SlabList::<u64>::new(config).unwrap();
///
/// let a = arena.insert(10);
/// let b = arena.insert(20);
///
/// assert_eq!(*a, 10);
/// assert_eq!(*b, 20);
/// assert_eq!(arena.remove(a), 10);
/// assert_eq!(*b, 20);
/// ```
pub struct SlabList<T, A = Global>
where
    A: Allocator,
{
    /// 链表头部；为空表示还没有分配任何 Chunk。
    head_: *mut ChunkHeader,

    /// 链表尾部。
    tail_: *mut ChunkHeader,

    /// 创建时使用的 Chunk 配置。
    config_: SlabListConfig,

    /// 分配 Chunk 内存所使用的 allocator。
    allocator_: A,

    /// 表明 `SlabList<T>` 拥有 `T`，并参与 drop check。
    _marker_: PhantomData<Slot<T>>,
}

impl<T> SlabList<T, Global> {
    /// 使用默认的 [`Global`] allocator 和给定 Chunk 配置创建一个空的 `SlabList`。
    ///
    /// 创建时不会立即分配 Chunk；第一次 `insert` 时才分配第一个 Chunk。
    ///
    /// # Errors
    ///
    /// 当配置的 Chunk 字节数无法容纳至少一个 `Slot<T>` 时返回
    /// [`SlabListError::ChunkTooSmall`]；当计算出的槽位数超过
    /// [`SlabListConfig::MAX_SLOTS_PER_CHUNK`] 时返回
    /// [`SlabListError::TooManySlots`]；布局无法表示时返回
    /// [`SlabListError::LayoutOverflow`]。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{SlabList, SlabListConfig};
    ///
    /// let arena = SlabList::<u64>::new(SlabListConfig::new(4096)).unwrap();
    /// assert!(arena.slots_per_chunk() > 0);
    /// ```
    pub fn new(config: SlabListConfig) -> Result<Self, SlabListError> {
        SlabList::new_in(config, Global)
    }

    /// 使用默认的 [`Global`] allocator 和默认 Chunk 配置创建一个空的 `SlabList`。
    ///
    /// 默认单 Chunk 大小为 64 KiB，可通过 [`SlabListConfig::default`] 获取该配置。
    ///
    /// # Errors
    ///
    /// 与 [`SlabList::new`] 相同。
    pub fn with_default_config() -> Result<Self, SlabListError> {
        SlabList::new(SlabListConfig::default())
    }
}

impl<T, A> SlabList<T, A>
where
    A: Allocator,
{
    /// 使用指定的 allocator 和 Chunk 配置创建一个空的 `SlabList`。
    ///
    /// 创建时不会立即分配 Chunk；第一次 `insert` 时才分配第一个 Chunk。
    ///
    /// # Errors
    ///
    /// 当配置的 Chunk 字节数无法容纳至少一个 `Slot<T>` 时返回
    /// [`SlabListError::ChunkTooSmall`]；当计算出的槽位数超过
    /// [`SlabListConfig::MAX_SLOTS_PER_CHUNK`] 时返回
    /// [`SlabListError::TooManySlots`]；布局无法表示时返回
    /// [`SlabListError::LayoutOverflow`]。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{SlabList, SlabListConfig};
    /// use std::alloc::Global;
    ///
    /// let arena = SlabList::<u64, _>::new_in(SlabListConfig::new(4096), Global).unwrap();
    /// assert_eq!(arena.config().chunk_bytes(), 4096);
    /// ```
    pub fn new_in(config: SlabListConfig, allocator: A) -> Result<Self, SlabListError> {
        config.chunk_layout_::<T>()?;

        Ok(SlabList {
            head_: ptr::null_mut(),
            tail_: ptr::null_mut(),
            config_: config,
            allocator_: allocator,
            _marker_: PhantomData,
        })
    }

    /// 使用默认 Chunk 配置和指定的 allocator 创建一个空的 `SlabList`。
    ///
    /// # Errors
    ///
    /// 与 [`SlabList::new_in`] 相同。
    pub fn with_default_config_in(allocator: A) -> Result<Self, SlabListError> {
        SlabList::new_in(SlabListConfig::default(), allocator)
    }

    /// 返回创建时使用的 Chunk 配置。
    pub const fn config(&self) -> SlabListConfig {
        self.config_
    }

    /// 返回创建时指定的 allocator 的引用。
    pub const fn allocator(&self) -> &A {
        &self.allocator_
    }

    /// 返回每个 Chunk 的槽位数。
    ///
    /// 该值由配置和 `T` 的布局实时计算，不作为独立字段缓存在 `SlabList` 中。
    pub fn slots_per_chunk(&self) -> usize {
        self.chunk_layout_().slots_per_chunk_ as usize
    }

    /// 从配置和 `T` 计算 Chunk 布局。
    ///
    /// `new_in` 已经验证过配置，因此这里不应失败。
    fn chunk_layout_(&self) -> ChunkLayout {
        match self.config_.chunk_layout_::<T>() {
            Ok(layout) => layout,
            Err(_) => unreachable!("SlabList 创建时已校验 Chunk 配置"),
        }
    }

    /// 在 arena 中写入一个元素，并返回指向它的句柄。
    ///
    /// 如果链表头部 Chunk 已满，会先分配一个新 Chunk 并放到链表头部。
    /// 元素写入后地址保持稳定，直到它被 `remove` 或 arena 被 drop。
    ///
    /// # Panics
    ///
    /// 本方法不会主动 panic。底层分配失败时按全局 allocator 的错误处理器处理。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{SlabList, SlabListConfig};
    ///
    /// let mut arena = SlabList::<u64>::new(SlabListConfig::new(4096)).unwrap();
    /// let a = arena.insert(1);
    /// let b = arena.insert(2);
    ///
    /// assert_eq!(*a, 1);
    /// assert_eq!(*b, 2);
    /// ```
    pub fn insert(&mut self, value: T) -> SlotPtr<T> {
        let chunk_layout = self.chunk_layout_();
        let chunk = self.ensure_free_chunk_(chunk_layout);

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
    /// 槽位会被压入所属 Chunk 的空闲链表头部；同时该 Chunk 会被移到
    /// 整个 Chunk 链表的头部，以便后续 `insert` 优先复用它。
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
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{SlabList, SlabListConfig};
    ///
    /// let mut arena = SlabList::<u64>::new(SlabListConfig::new(4096)).unwrap();
    /// let a = arena.insert(1);
    /// let b = arena.insert(2);
    ///
    /// assert_eq!(arena.remove(a), 1);
    /// assert_eq!(*b, 2);
    /// ```
    pub fn remove(&mut self, ptr: SlotPtr<T>) -> T {
        let chunk_layout = self.chunk_layout_();

        // SAFETY:
        // - SlotPtr 的安全约定保证 ptr 指向本 arena 中一个已初始化的槽位。
        // - 槽位基址就是 T 的地址，因此可以 cast 回 Slot<T> 并读取元数据。
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

            self.move_to_head_(chunk);

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
    fn ensure_free_chunk_(&mut self, chunk_layout: ChunkLayout) -> *mut ChunkHeader {
        let must_alloc = if self.head_.is_null() {
            true
        } else {
            // SAFETY: head_ 非空时指向有效 Chunk。
            unsafe { (*self.head_).free_count_ == 0 }
        };

        if must_alloc {
            self.alloc_chunk_front_(chunk_layout);
        }

        debug_assert!(!self.head_.is_null());
        // SAFETY: 上面保证 head_ 非空，且 alloc_chunk_front_ 分配的 Chunk
        // 至少有一个空闲槽位。
        self.head_
    }

    /// 使用本 arena 的 allocator 分配一个新 Chunk 并插入链表头部。
    fn alloc_chunk_front_(&mut self, chunk_layout: ChunkLayout) -> *mut ChunkHeader {
        // SAFETY: chunk_layout 由 config.chunk_layout_::<T>() 成功计算，
        // 且 allocator_ 就是本 arena 构造时指定的 allocator。
        let chunk = unsafe { alloc_chunk_::<T, A>(&self.allocator_, chunk_layout) };

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

impl<T, A> Drop for SlabList<T, A>
where
    A: Allocator,
{
    fn drop(&mut self) {
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

                dealloc_chunk_(&self.allocator_, chunk, chunk_layout.layout_);
                chunk = next;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::{
        alloc::{AllocError, Allocator, Layout},
        cell::Cell,
        mem::size_of,
        ptr::NonNull,
    };
    use std::rc::Rc;

    use super::super::chunk_::slot_offset_;
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

    /// 构造一个刚好容纳 `slots` 个槽位的 Chunk 配置。
    fn config_for_slots_<T>(slots: usize) -> SlabListConfig {
        let offset = slot_offset_::<T>().expect("Chunk header 的偏移应当可以计算");
        let stride = size_of::<Slot<T>>();
        SlabListConfig::new(offset + stride * slots)
    }

    /// 测试目标：验证 Chunk 字节数不足时拒绝创建 SlabList。
    /// Test goal: Verify that a chunk configuration too small for one slot is rejected.
    ///
    /// 测试手段：分别用 0 字节和不足 `header + 一个槽位` 的字节数构造
    /// `SlabList<u32>`。
    /// Test method: Build `SlabList<u32>` with zero bytes and with fewer bytes than
    /// `header + one slot`.
    ///
    /// 判定标准：两次都返回 `SlabListError::ChunkTooSmall`。
    /// Assertions: Both calls return `SlabListError::ChunkTooSmall`.
    #[test]
    fn test_new_rejects_too_small_chunk() {
        assert_eq!(
            SlabList::<u32>::new(SlabListConfig::new(0)).err(),
            Some(SlabListError::ChunkTooSmall)
        );

        let offset = slot_offset_::<u32>().expect("Chunk header 的偏移应当可以计算");
        let stride = size_of::<Slot<u32>>();
        let bytes = offset + stride - 1;

        assert_eq!(
            SlabList::<u32>::new(SlabListConfig::new(bytes)).err(),
            Some(SlabListError::ChunkTooSmall)
        );
    }

    /// 测试目标：验证槽位数超过 2^15 时拒绝创建 SlabList。
    /// Test goal: Verify that a chunk configuration implying more than 2^15 slots is rejected.
    ///
    /// 测试手段：为 `u8` 构造一个刚好能容纳
    /// `MAX_SLOTS_PER_CHUNK + 1` 个槽位的 Chunk 字节数。
    /// Test method: For `u8`, build chunk bytes that fit exactly
    /// `MAX_SLOTS_PER_CHUNK + 1` slots.
    ///
    /// 判定标准：创建返回 `SlabListError::TooManySlots`。
    /// Assertions: Creation returns `SlabListError::TooManySlots`.
    #[test]
    fn test_new_rejects_too_many_slots() {
        let offset = slot_offset_::<u8>().expect("Chunk header 的偏移应当可以计算");
        let stride = size_of::<Slot<u8>>();
        let slots = SlabListConfig::MAX_SLOTS_PER_CHUNK + 1;
        let bytes = offset + stride * slots;

        assert_eq!(
            SlabList::<u8>::new(SlabListConfig::new(bytes)).err(),
            Some(SlabListError::TooManySlots)
        );
    }

    /// 测试目标：验证基本插入、Deref/DerefMut 以及乱序归还。
    /// Test goal: Verify basic insertion, Deref/DerefMut, and out-of-order removal.
    ///
    /// 测试手段：在可容纳 4 个槽位的 arena 中插入 3 个 `u32`，通过句柄修改其中一个，
    /// 然后乱序移除并重新插入。
    /// Test method: Insert three `u32` values into an arena with four slots, mutate one
    /// through its handle, then remove out of order and insert again.
    ///
    /// 判定标准：句柄读写得到预期值；乱序移除返回正确的原值；重新插入后所有槽位
    /// 仍可正常使用。
    /// Assertions: Handles read and write expected values; out-of-order removals return
    /// the correct original values; all slots remain usable after reinsertion.
    #[test]
    fn test_insert_deref_and_remove_out_of_order() {
        let mut arena = SlabList::<u32>::new(config_for_slots_::<u32>(4)).expect("配置应当有效");

        assert_eq!(arena.slots_per_chunk(), 4);

        let mut a = arena.insert(10);
        let b = arena.insert(20);
        let c = arena.insert(30);

        assert_eq!(*a, 10);
        assert_eq!(*b, 20);
        assert_eq!(*c, 30);

        *a += 5;
        assert_eq!(*a, 15);

        assert_eq!(arena.remove(b), 20);

        let d = arena.insert(40);
        assert_eq!(*d, 40);

        assert_eq!(arena.remove(a), 15);
        assert_eq!(arena.remove(c), 30);
        assert_eq!(arena.remove(d), 40);

        let e = arena.insert(50);
        assert_eq!(*e, 50);
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
        let mut arena = SlabList::<u32>::new(config_for_slots_::<u32>(4)).expect("配置应当有效");

        let a = arena.insert(1);
        let b = arena.insert(2);

        assert_eq!(arena.remove(a), 1);
        let c = arena.insert(3);
        assert_eq!(a.as_ptr(), c.as_ptr());

        assert_eq!(arena.remove(b), 2);
        let d = arena.insert(4);
        assert_eq!(b.as_ptr(), d.as_ptr());
    }

    /// 测试目标：验证 Chunk 满后扩容，以及归还槽位后 Chunk 移回头部并被优先复用。
    /// Test goal: Verify chunk growth and that a chunk is moved to the head and reused
    /// after a slot is freed.
    ///
    /// 测试手段：使用每 Chunk 2 个槽位的配置插入 5 个元素，然后释放第一个元素，
    /// 再插入第 6 个元素；随后释放已经位于链表尾部的第 6 个元素，再插入第 7 个元素。
    /// Test method: Use two slots per chunk, insert five elements, free the first one,
    /// then insert a sixth element; after that, free the sixth element (now in the tail
    /// chunk) and insert a seventh element.
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
        let mut arena = SlabList::<u32>::new(config_for_slots_::<u32>(2)).expect("配置应当有效");

        let a = arena.insert(0);
        let _b = arena.insert(1);
        let _c = arena.insert(2);
        let _d = arena.insert(3);
        let _e = arena.insert(4);

        assert_eq!(arena.slots_per_chunk(), 2);
        assert_eq!(arena.chunk_count_(), 3);
        assert_eq!(arena.chunk_count_() * arena.slots_per_chunk(), 6);

        assert_eq!(arena.remove(a), 0);
        assert_eq!(arena.chunk_count_(), 3);
        assert_eq!(arena.head_free_count_(), 1);

        let f = arena.insert(5);
        assert_eq!(arena.chunk_count_(), 3);
        assert_eq!(arena.chunk_count_() * arena.slots_per_chunk(), 6);
        assert_eq!(f.as_ptr(), a.as_ptr());

        // f 所在的 Chunk 刚刚被移到尾部；再次释放它时，该 Chunk 应移回头部。
        // The chunk holding f was just moved to the tail; freeing it should move the
        // chunk back to the head.
        assert_eq!(arena.remove(f), 5);
        assert_eq!(arena.head_free_count_(), 1);

        let g = arena.insert(6);
        assert_eq!(g.as_ptr(), f.as_ptr());
        assert_eq!(arena.chunk_count_(), 3);
        assert_eq!(arena.chunk_count_() * arena.slots_per_chunk(), 6);
    }

    /// 测试目标：验证 `drop` 只析构仍存活的元素，不会重复析构已归还槽位。
    /// Test goal: Verify that `drop` drops only live elements and never double-drops a
    /// freed slot.
    ///
    /// 测试手段：用带析构计数器的 `DropProbe` 在每 Chunk 2 个槽位的 arena 中插入
    /// 3 个元素（跨越两个 Chunk），移除其中一个并立即析构，然后让 arena 离开作用域。
    /// Test method: Insert three `DropProbe` values across two chunks (two slots per
    /// chunk), remove one and drop it immediately, then let the arena go out of scope.
    ///
    /// 判定标准：移除后计数器为 1；arena drop 后计数器为 3，说明只有两个存活元素
    /// 被 arena 析构，空闲槽位没有被重复析构。
    /// Assertions: The counter is 1 after the explicit removal and 3 after the arena is
    /// dropped, meaning only the two live elements were dropped by the arena.
    #[test]
    fn test_drop_drops_only_live_elements() {
        struct DropProbe<'a> {
            counter_: &'a Cell<usize>,
            value_: u32,
        }

        impl Drop for DropProbe<'_> {
            fn drop(&mut self) {
                self.counter_.set(self.counter_.get() + 1);
            }
        }

        let counter = Cell::new(0);

        {
            let mut arena = SlabList::<DropProbe<'_>>::new(config_for_slots_::<DropProbe<'_>>(2))
                .expect("配置应当有效");

            let a = arena.insert(DropProbe {
                counter_: &counter,
                value_: 1,
            });
            let _b = arena.insert(DropProbe {
                counter_: &counter,
                value_: 2,
            });
            let _c = arena.insert(DropProbe {
                counter_: &counter,
                value_: 3,
            });

            let removed = arena.remove(a);
            assert_eq!(removed.value_, 1);
            drop(removed);
            assert_eq!(counter.get(), 1);
        }

        assert_eq!(counter.get(), 3);
    }

    /// 测试目标：验证零大小类型也能获得互相区分且可乱序归还的槽位。
    /// Test goal: Verify that zero-sized types get distinct, individually reclaimable slots.
    ///
    /// 测试手段：在可容纳 4 个槽位的 `SlabList<()>` 中插入 3 个 `()`，移除中间一个，
    /// 再插入一个。
    /// Test method: Insert three `()` values into a four-slot `SlabList<()>`, remove the
    /// middle one, and insert another.
    ///
    /// 判定标准：三个初始句柄地址互不相同；重新插入复用被释放槽位的地址。
    /// Assertions: The three initial handles have distinct addresses; the new insert
    /// reuses the freed slot address.
    #[test]
    fn test_zero_sized_type_slots() {
        let mut arena = SlabList::<()>::new(config_for_slots_::<()>(4)).expect("配置应当有效");

        let a = arena.insert(());
        let b = arena.insert(());
        let c = arena.insert(());

        assert_ne!(a.as_ptr(), b.as_ptr());
        assert_ne!(b.as_ptr(), c.as_ptr());

        assert_eq!(arena.remove(b), ());
        let d = arena.insert(());

        assert_eq!(d.as_ptr(), b.as_ptr());
    }

    /// 测试目标：验证对已归还槽位再次 `remove` 会 panic。
    /// Test goal: Verify that removing the same slot twice panics.
    ///
    /// 测试手段：插入一个元素，归还一次，然后用同一个句柄再次归还。
    /// Test method: Insert one element, remove it once, then remove it again with the
    /// same handle.
    ///
    /// 判定标准：第二次调用触发 `remove called with a freed or invalid SlotPtr`
    /// 对应的 panic。
    /// Assertions: The second call panics with `remove called with a freed or invalid
    /// SlotPtr`.
    #[test]
    #[should_panic(expected = "freed or invalid SlotPtr")]
    fn test_double_remove_panics() {
        let mut arena = SlabList::<u32>::new(config_for_slots_::<u32>(2)).expect("配置应当有效");

        let a = arena.insert(1);
        assert_eq!(arena.remove(a), 1);
        let _ = arena.remove(a);
    }

    /// 测试目标：验证默认配置可以创建 arena，且初始状态正确。
    /// Test goal: Verify that the default configuration creates a valid arena with the
    /// expected initial state.
    ///
    /// 测试手段：使用 `with_default_config` 创建 `SlabList<u32>`。
    /// Test method: Create `SlabList<u32>` through `with_default_config`.
    ///
    /// 判定标准：配置字节数与默认配置一致；每 Chunk 槽位数大于 0；尚未分配 Chunk。
    /// Assertions: The configured chunk bytes match the default configuration; slots per
    /// chunk are positive; no chunk is allocated yet.
    #[test]
    fn test_default_config() {
        let arena = SlabList::<u32>::with_default_config().expect("默认配置应当有效");

        assert_eq!(
            arena.config().chunk_bytes(),
            SlabListConfig::default().chunk_bytes()
        );
        assert!(arena.slots_per_chunk() > 0);
        assert_eq!(
            arena
                .config()
                .slots_per_chunk::<u32>()
                .expect("配置应当有效"),
            arena.slots_per_chunk()
        );
        assert_eq!(arena.chunk_count_(), 0);
    }

    /// 测试目标：验证外部 `Allocator` 被用于 Chunk 的分配与释放。
    /// Test goal: Verify that an external `Allocator` is used for chunk allocation and
    /// deallocation.
    ///
    /// 测试手段：用记录分配/释放次数的 `CountingAlloc` 创建每 Chunk 2 个槽位的 arena，
    /// 插入 3 个元素触发两次 Chunk 分配，然后让 arena 离开作用域。
    /// Test method: Create a two-slot-per-chunk arena with a `CountingAlloc` that records
    /// allocation/deallocation counts, insert three elements to trigger two chunk
    /// allocations, then let the arena go out of scope.
    ///
    /// 判定标准：创建后分配次数为 0；第一次插入后为 1；第三次插入后为 2；
    /// arena drop 后释放次数为 2。
    /// Assertions: No allocation occurs on construction; one after the first insert; two
    /// after the third insert; and two deallocations after the arena is dropped.
    #[test]
    fn test_external_allocator_is_used() {
        let allocs = Rc::new(Cell::new(0));
        let deallocs = Rc::new(Cell::new(0));
        let allocator = CountingAlloc {
            allocs_: Rc::clone(&allocs),
            deallocs_: Rc::clone(&deallocs),
        };

        {
            let mut arena = SlabList::<u32, _>::new_in(config_for_slots_::<u32>(2), allocator)
                .expect("配置应当有效");

            assert_eq!(allocs.get(), 0);

            let a = arena.insert(1);
            assert_eq!(*a, 1);
            assert_eq!(allocs.get(), 1);

            let _b = arena.insert(2);
            assert_eq!(allocs.get(), 1);

            let _c = arena.insert(3);
            assert_eq!(allocs.get(), 2);
            assert_eq!(arena.allocator().allocs_.get(), 2);

            assert_eq!(deallocs.get(), 0);
        }

        assert_eq!(deallocs.get(), 2);
    }
}
