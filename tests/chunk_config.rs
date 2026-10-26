//! 自定义 `TrConfig` / Chunk 大小的集成测试。
//!
//! 这些测试只通过 crate 根公开的 `TrConfig`、`SlabListConfig` 与树类型验证
//! Chunk 大小配置，不访问内部 `SlabList`。

use core::{
    alloc::{AllocError, Allocator, Layout},
    cell::{Cell, RefCell},
    ptr::NonNull,
};
use std::{alloc::Global, rc::Rc, vec::Vec};

use bplustree_cmp::{BPlusTree, BTree, CHUNK_SIZE_4K, OrdComparer, SlabListConfig, TrConfig};

/// 记录每次 Chunk 分配字节数与释放次数、但不实现 `Clone` 的 allocator。
struct RecordingAlloc {
    sizes_: Rc<RefCell<Vec<usize>>>,
    deallocs_: Rc<Cell<usize>>,
}

// SAFETY: RecordingAlloc 把所有分配/释放委托给 Global，只额外记录 size 和
// 释放次数；因此它满足 Allocator 对指针有效性、布局匹配和分配失败的要求。
unsafe impl Allocator for RecordingAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.sizes_.borrow_mut().push(layout.size());
        Global.allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: 调用者保证 ptr/layout 来自本 allocator 的 allocate；
        // 本实现将分配委托给 Global。
        unsafe { Global.deallocate(ptr, layout) };
        self.deallocs_.set(self.deallocs_.get() + 1);
    }
}

type SmallBPlus<A> = BPlusTree<4, OrdComparer<i32>, i32, i32, SlabListConfig<CHUNK_SIZE_4K, A>>;
type SmallBTree<A> = BTree<4, OrdComparer<i32>, i32, i32, SlabListConfig<CHUNK_SIZE_4K, A>>;

/// 测试目标：验证 B+ Tree 的 data/node 两个 arena 都使用自定义 Chunk 大小。
/// Test goal: Verify that both B+ Tree arenas (data and node) use the configured
/// custom chunk size.
///
/// 测试手段：用记录 allocation size 的 `RecordingAlloc` 构造
/// `SlabListConfig<CHUNK_SIZE_4K, _>` 的 B+ Tree，插入足够多元素触发多个 Chunk 分配，
/// 然后检查所有分配的字节数。
/// Test method: Build a B+ Tree with `SlabListConfig<CHUNK_SIZE_4K, _>` using a
/// `RecordingAlloc` that records allocation sizes, insert enough elements to trigger
/// several chunks, and inspect every recorded allocation size.
///
/// 判定标准：至少发生一次 Chunk 分配，且所有分配的字节数都等于 4096；树 drop 后
/// 释放次数等于分配次数。
/// Assertions: At least one chunk allocation happens, every allocation size is 4096,
/// and after the tree is dropped the deallocation count equals the allocation count.
#[test]
fn test_bplus_tree_custom_chunk_size() {
    let sizes = Rc::new(RefCell::new(Vec::new()));
    let deallocs = Rc::new(Cell::new(0));
    let allocator = RecordingAlloc {
        sizes_: Rc::clone(&sizes),
        deallocs_: Rc::clone(&deallocs),
    };

    {
        let mut tree =
            SmallBPlus::<RecordingAlloc>::new_in(allocator).expect("4096 字节配置应当有效");
        let count = if cfg!(miri) { 64 } else { 4096 };
        for key in 0..count {
            tree.insert(key, key);
        }

        let recorded = sizes.borrow();
        assert!(!recorded.is_empty(), "至少应分配一个 Chunk");
        assert!(
            recorded.iter().all(|&size| size == 4096),
            "所有 Chunk 都应为 4096 字节: {recorded:?}"
        );
    }

    assert_eq!(deallocs.get(), sizes.borrow().len());
}

/// 测试目标：验证 B Tree 的所有 Chunk 使用自定义大小。
/// Test goal: Verify that every B Tree chunk uses the configured custom size.
///
/// 测试手段：用记录 allocation size 的 `RecordingAlloc` 构造
/// `SlabListConfig<CHUNK_SIZE_4K, _>` 的 B Tree，插入足够多元素触发多个 Chunk，然后检查
/// 所有分配的字节数。
/// Test method: Build a B Tree with `SlabListConfig<CHUNK_SIZE_4K, _>` using a
/// `RecordingAlloc` that records allocation sizes, insert enough elements to trigger
/// several chunks, and inspect every recorded allocation size.
///
/// 判定标准：至少发生一次 Chunk 分配，且所有分配字节数都等于 4096；树 drop 后
/// 释放次数等于分配次数。
/// Assertions: At least one chunk allocation happens, every allocation size is 4096,
/// and after the tree is dropped the deallocation count equals the allocation count.
#[test]
fn test_btree_custom_chunk_size() {
    let sizes = Rc::new(RefCell::new(Vec::new()));
    let deallocs = Rc::new(Cell::new(0));
    let allocator = RecordingAlloc {
        sizes_: Rc::clone(&sizes),
        deallocs_: Rc::clone(&deallocs),
    };

    {
        let mut tree =
            SmallBTree::<RecordingAlloc>::new_in(allocator).expect("4096 字节配置应当有效");
        let count = if cfg!(miri) { 64 } else { 4096 };
        for key in 0..count {
            tree.insert(key, key);
        }

        let recorded = sizes.borrow();
        assert!(!recorded.is_empty(), "至少应分配一个 Chunk");
        assert!(
            recorded.iter().all(|&size| size == 4096),
            "所有 Chunk 都应为 4096 字节: {recorded:?}"
        );
    }

    assert_eq!(deallocs.get(), sizes.borrow().len());
}

/// 配置类型：通过公开的 `TrConfig` 自行指定 8192 字节 Chunk。
struct CustomConfig;

impl TrConfig for CustomConfig {
    type Allocator = RecordingAlloc;

    const CHUNK_SIZE: usize = 8192;
}

/// 测试目标：验证用户可以自行实现公开的 `TrConfig`，而不局限于内置配置。
/// Test goal: Verify that users can implement the public `TrConfig` themselves instead
/// of being limited to built-in configurations.
///
/// 测试手段：定义 `CustomConfig` 实现 `TrConfig`，用其构造 B+ Tree 并记录所有
/// Chunk 分配的字节数。
/// Test method: Define `CustomConfig` implementing `TrConfig`, build a B+ Tree with it,
/// and record every chunk allocation size.
///
/// 判定标准：所有 Chunk 分配大小都等于自定义的 8192；树 drop 后释放次数等于分配
/// 次数。
/// Assertions: Every chunk allocation size is 8192 and, after the tree is dropped,
/// deallocations equal allocations.
#[test]
fn test_custom_trconfig_implementation() {
    type CustomTree = BPlusTree<4, OrdComparer<i32>, i32, i32, CustomConfig>;

    let sizes = Rc::new(RefCell::new(Vec::new()));
    let deallocs = Rc::new(Cell::new(0));
    let allocator = RecordingAlloc {
        sizes_: Rc::clone(&sizes),
        deallocs_: Rc::clone(&deallocs),
    };

    {
        let mut tree = CustomTree::new_in(allocator).expect("自定义配置应当有效");
        let count = if cfg!(miri) { 64 } else { 1024 };
        for key in 0..count {
            tree.insert(key, key);
        }

        let recorded = sizes.borrow();
        assert!(!recorded.is_empty(), "至少应分配一个 Chunk");
        assert!(
            recorded.iter().all(|&size| size == 8192),
            "所有 Chunk 都应为 8192 字节: {recorded:?}"
        );
    }

    assert_eq!(deallocs.get(), sizes.borrow().len());
}

/// 测试目标：验证自定义 Chunk 大小可以与默认 `Global` allocator 一起使用 `new`。
/// Test goal: Verify that a custom chunk size can be used with the default `Global`
/// allocator through `new`.
///
/// 测试手段：为 `SlabListConfig<CHUNK_SIZE_4K>` 的 B+ Tree 和 B Tree 调用 `new()` 并插入元素。
/// Test method: Call `new()` on B+ Tree and B Tree values configured with
/// `SlabListConfig<CHUNK_SIZE_4K>` and insert elements.
///
/// 判定标准：两棵树都构造成功并可正常查询。
/// Assertions: Both trees are constructed successfully and can be queried.
#[test]
fn test_custom_chunk_with_global_allocator() {
    type GlobalBPlus = BPlusTree<4, OrdComparer<i32>, i32, i32, SlabListConfig<CHUNK_SIZE_4K>>;
    type GlobalBTree = BTree<4, OrdComparer<i32>, i32, i32, SlabListConfig<CHUNK_SIZE_4K>>;

    let mut bplus = GlobalBPlus::new().expect("4096 字节配置应当有效");
    let mut btree = GlobalBTree::new().expect("4096 字节配置应当有效");

    bplus.insert(1, 10);
    btree.insert(1, 10);
    assert_eq!(bplus.get(1), Some((&1, &10)));
    assert_eq!(btree.get(1), Some((&1, &10)));

    let collected_bplus: GlobalBPlus = (0..8).map(|key| (key, key)).collect();
    let collected_btree: GlobalBTree = (0..8).map(|key| (key, key)).collect();
    assert_eq!(collected_bplus.len(), 8);
    assert_eq!(collected_btree.len(), 8);
}

/// 测试目标：验证不足以容纳一个槽位的 Chunk 配置在树构造时返回错误。
/// Test goal: Verify that a chunk configuration too small for one slot is rejected when
/// constructing a tree.
///
/// 测试手段：用 `SlabListConfig<1>` 分别构造 B+ Tree 与 B Tree。
/// Test method: Construct B+ Tree and B Tree values with `SlabListConfig<1>`.
///
/// 判定标准：两次构造都返回 `Err`，而不是 panic。
/// Assertions: Both constructions return `Err` rather than panicking.
#[test]
fn test_invalid_chunk_size_returns_tree_error() {
    type TinyBPlus = BPlusTree<4, OrdComparer<i32>, i32, i32, SlabListConfig<1>>;
    type TinyBTree = BTree<4, OrdComparer<i32>, i32, i32, SlabListConfig<1>>;

    assert!(TinyBPlus::new().is_err());
    assert!(TinyBTree::new().is_err());
}
