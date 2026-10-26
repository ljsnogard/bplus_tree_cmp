//! 公开 allocator 泛型参数的集成测试。
//!
//! 这些测试只通过 crate 根导出的类型和方法验证 allocator 行为，不访问内部
//! `SlabList`。关键点是使用一个 **没有实现 `Clone`** 的 allocator，以证明
//! 两棵树都自己持有 allocator，并按需把它借给内部 arena。

use core::{
    alloc::{AllocError, Allocator, Layout},
    cell::Cell,
    ptr::NonNull,
};
use std::{alloc::Global, rc::Rc};

use bplustree_cmp::{BPlusTree, BTree};

/// 记录分配/释放次数、但故意不实现 `Clone` 的 allocator。
struct CountingAlloc {
    allocs_: Rc<Cell<usize>>,
    deallocs_: Rc<Cell<usize>>,
}

// SAFETY: CountingAlloc 把所有分配/释放委托给 Global，只额外记录次数；
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

/// 测试目标：验证 B+ Tree 使用公开的 allocator 泛型参数，且不要求 `A: Clone`。
/// Test goal: Verify that B+ Tree uses the public allocator generic parameter and
/// does not require `A: Clone`.
///
/// 测试手段：用一个没有实现 `Clone` 的 `CountingAlloc` 通过 `new_in` 创建树，
/// 插入足够多的元素触发内部 arena 分配 Chunk，然后让树离开作用域。
/// Test method: Create a tree with `new_in` from a `CountingAlloc` that does not
/// implement `Clone`, insert enough entries to make the internal arenas allocate
/// chunks, and let the tree go out of scope.
///
/// 判定标准：构造后分配次数为 0；插入后分配次数大于 0；树 drop 后释放次数等于
/// 分配次数，说明 B+ Tree 的数据 arena 与节点 arena 都使用了该 allocator，
/// 并且都在 allocator 仍存活时归还了 Chunk。
/// Assertions: No allocation happens during construction; at least one allocation
/// happens after insertion; after the tree is dropped, the deallocation count equals
/// the allocation count, proving both the data and node arenas used the allocator and
/// returned their chunks while it was still alive.
#[test]
fn test_bplus_tree_uses_non_clone_allocator() {
    let allocs = Rc::new(Cell::new(0));
    let deallocs = Rc::new(Cell::new(0));
    let allocator = CountingAlloc {
        allocs_: Rc::clone(&allocs),
        deallocs_: Rc::clone(&deallocs),
    };

    {
        let mut tree =
            BPlusTree::<4, _, i32, i32, _>::new_in(allocator).expect("默认 SlabList 配置应当有效");

        assert_eq!(allocs.get(), 0);

        for key in 0..20_000i32 {
            tree.insert(key, key);
        }

        assert!(allocs.get() > 0);
        assert_eq!(deallocs.get(), 0);
    }

    assert!(allocs.get() > 0);
    assert_eq!(deallocs.get(), allocs.get());
}

/// 测试目标：验证 B Tree 使用公开的 allocator 泛型参数，且不要求 `A: Clone`。
/// Test goal: Verify that B Tree uses the public allocator generic parameter and does
/// not require `A: Clone`.
///
/// 测试手段：用一个没有实现 `Clone` 的 `CountingAlloc` 通过 `new_in` 创建树，
/// 插入足够多的元素触发内部 arena 分配 Chunk，然后让树离开作用域。
/// Test method: Create a tree with `new_in` from a `CountingAlloc` that does not
/// implement `Clone`, insert enough entries to make the internal arena allocate chunks,
/// and let the tree go out of scope.
///
/// 判定标准：构造后分配次数为 0；插入后分配次数大于 0；树 drop 后释放次数等于
/// 分配次数，说明 B Tree 的节点 arena 使用了该 allocator，并在 allocator 仍存活
/// 时归还了 Chunk。
/// Assertions: No allocation happens during construction; at least one allocation
/// happens after insertion; after the tree is dropped, the deallocation count equals
/// the allocation count, proving the node arena used the allocator and returned its
/// chunks while it was still alive.
#[test]
fn test_btree_uses_non_clone_allocator() {
    let allocs = Rc::new(Cell::new(0));
    let deallocs = Rc::new(Cell::new(0));
    let allocator = CountingAlloc {
        allocs_: Rc::clone(&allocs),
        deallocs_: Rc::clone(&deallocs),
    };

    {
        let mut tree =
            BTree::<4, _, i32, i32, _>::new_in(allocator).expect("默认 SlabList 配置应当有效");

        assert_eq!(allocs.get(), 0);

        for key in 0..20_000i32 {
            tree.insert(key, key);
        }

        assert!(allocs.get() > 0);
        assert_eq!(deallocs.get(), 0);
    }

    assert!(allocs.get() > 0);
    assert_eq!(deallocs.get(), allocs.get());
}

/// 测试目标：验证带 comparer 的构造器同样接受自定义 allocator。
/// Test goal: Verify that constructors taking a comparer also accept a custom
/// allocator.
///
/// 测试手段：使用 `with_comparer_in`、一个反向 comparer 和自定义 allocator
/// 创建 B+ Tree，插入若干元素后让树离开作用域。
/// Test method: Create a B+ Tree with `with_comparer_in`, a reverse comparer, and a
/// custom allocator; insert several entries and let the tree go out of scope.
///
/// 判定标准：查询返回插入的 value；树 drop 后释放次数等于分配次数。
/// Assertions: Lookup returns the inserted value, and after the tree is dropped the
/// deallocation count equals the allocation count.
#[test]
fn test_with_comparer_in_accepts_custom_allocator() {
    use core::cmp::Ordering;

    use bplustree_cmp::TrComparer;

    struct ReverseComparer;

    impl TrComparer<i32> for ReverseComparer {
        fn compare(&self, a: &i32, b: &i32) -> Ordering {
            b.cmp(a)
        }
    }

    let allocs = Rc::new(Cell::new(0));
    let deallocs = Rc::new(Cell::new(0));
    let allocator = CountingAlloc {
        allocs_: Rc::clone(&allocs),
        deallocs_: Rc::clone(&deallocs),
    };

    {
        let mut tree = BPlusTree::<4, _, i32, i32, _>::with_comparer_in(ReverseComparer, allocator)
            .expect("默认 SlabList 配置应当有效");

        for key in 0..128i32 {
            tree.insert(key, key * 2);
        }

        assert_eq!(tree.get(7), Some((&7, &14)));
    }

    assert_eq!(deallocs.get(), allocs.get());
}

/// 测试目标：验证 B+ Tree 的 `range` / `range_mut` 与各类迭代 API 不产生临时堆分配。
/// Test goal: Verify that B+ Tree `range` / `range_mut` and the iterator APIs perform
/// no temporary heap allocation.
///
/// 测试手段：使用 `CountingAlloc` 构建并填充 B+ Tree，记录分配次数后依次执行
/// `range`（含空范围）、`range_mut`、`keys`、`values`、`values_mut` 与 `iter_mut`。
/// Test method: Build and fill a B+ Tree with `CountingAlloc`, record the allocation
/// count, then run `range` (including empty ranges), `range_mut`, `keys`, `values`,
/// `values_mut`, and `iter_mut`.
///
/// 判定标准：所有迭代期间分配次数完全不变；只有树自身的 Chunk 分配被计数，
/// 说明范围定位与迭代状态没有引入额外堆容器。
/// Assertions: The allocation count stays exactly unchanged during every iteration;
/// only the tree's own chunk allocations are counted, proving range positioning and
/// iteration state introduce no extra heap containers.
#[test]
fn test_bplus_tree_iteration_does_not_allocate() {
    let allocs = Rc::new(Cell::new(0));
    let deallocs = Rc::new(Cell::new(0));
    let allocator = CountingAlloc {
        allocs_: Rc::clone(&allocs),
        deallocs_: Rc::clone(&deallocs),
    };

    {
        let mut tree =
            BPlusTree::<4, _, i32, i32, _>::new_in(allocator).expect("默认 SlabList 配置应当有效");
        for key in 0..4096 {
            tree.insert(key, key);
        }

        let before = allocs.get();
        assert!(before > 0);

        let mut checksum = 0i64;
        for (key, value) in tree.range(100..300) {
            checksum += i64::from(*key) + i64::from(*value);
        }
        for (_, value) in tree.range_mut(400..500) {
            *value += 1;
        }
        for key in tree.keys() {
            checksum += i64::from(*key);
        }
        for value in tree.values() {
            checksum += i64::from(*value);
        }
        for value in tree.values_mut() {
            *value += 1;
            checksum += i64::from(*value);
        }
        for (key, value) in tree.iter_mut() {
            checksum += i64::from(*key) + i64::from(*value);
        }
        checksum += tree.range(..).count() as i64;
        checksum += tree.range(10..10).count() as i64;
        checksum += tree.range(9000..10000).count() as i64;
        assert!(checksum > 0);

        assert_eq!(allocs.get(), before);
        assert_eq!(deallocs.get(), 0);
    }

    assert_eq!(deallocs.get(), allocs.get());
}

/// 测试目标：验证 B Tree 的 `range` / `range_mut` 与各类迭代 API 不产生临时堆分配。
/// Test goal: Verify that B Tree `range` / `range_mut` and the iterator APIs perform no
/// temporary heap allocation.
///
/// 测试手段：使用 `CountingAlloc` 构建并填充 B Tree，记录分配次数后依次执行
/// `range`（含空范围）、`range_mut`、`keys`、`values`、`values_mut` 与 `iter_mut`。
/// Test method: Build and fill a B Tree with `CountingAlloc`, record the allocation
/// count, then run `range` (including empty ranges), `range_mut`, `keys`, `values`,
/// `values_mut`, and `iter_mut`.
///
/// 判定标准：所有迭代期间分配次数完全不变，且树 drop 后释放次数等于分配次数。
/// Assertions: The allocation count stays exactly unchanged during every iteration,
/// and after the tree is dropped the deallocation count equals the allocation count.
#[test]
fn test_btree_iteration_does_not_allocate() {
    let allocs = Rc::new(Cell::new(0));
    let deallocs = Rc::new(Cell::new(0));
    let allocator = CountingAlloc {
        allocs_: Rc::clone(&allocs),
        deallocs_: Rc::clone(&deallocs),
    };

    {
        let mut tree =
            BTree::<4, _, i32, i32, _>::new_in(allocator).expect("默认 SlabList 配置应当有效");
        for key in 0..4096 {
            tree.insert(key, key);
        }

        let before = allocs.get();
        assert!(before > 0);

        let mut checksum = 0i64;
        for (key, value) in tree.range(100..300) {
            checksum += i64::from(*key) + i64::from(*value);
        }
        for (_, value) in tree.range_mut(400..500) {
            *value += 1;
        }
        for key in tree.keys() {
            checksum += i64::from(*key);
        }
        for value in tree.values() {
            checksum += i64::from(*value);
        }
        for value in tree.values_mut() {
            *value += 1;
            checksum += i64::from(*value);
        }
        for (key, value) in tree.iter_mut() {
            checksum += i64::from(*key) + i64::from(*value);
        }
        checksum += tree.range(..).count() as i64;
        checksum += tree.range(10..10).count() as i64;
        checksum += tree.range(9000..10000).count() as i64;
        assert!(checksum > 0);

        assert_eq!(allocs.get(), before);
        assert_eq!(deallocs.get(), 0);
    }

    assert_eq!(deallocs.get(), allocs.get());
}
