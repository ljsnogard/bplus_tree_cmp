//! 树与迭代器的 Send / Sync 集成测试。
//!
//! 这些测试只验证 crate 根公开类型的线程安全 marker 与最基本的跨线程使用，
//! 不访问内部 arena。

use std::thread;

use bplustree_cmp::{
    BPlusTree, BTree, BTreeIter, BTreeIterMut, BTreeKeys, BTreeRangeIter, BTreeRangeIterMut,
    BTreeValues, BTreeValuesMut, CHUNK_SIZE_4K, IterMut, Keys, OrdComparer, RangeIter,
    RangeIterMut, SlabListConfig, TreeIter, Values, ValuesMut,
};

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

/// 测试目标：验证树与公开迭代器类型满足预期的 Send / Sync marker。
/// Test goal: Verify that the trees and public iterator types satisfy the expected
/// Send / Sync markers.
///
/// 测试手段：用泛型断言函数对默认配置、自定义 Chunk 配置的树，以及不可变/可变/
/// 范围迭代器、key/value 视图类型做编译期约束检查。
/// Test method: Use generic assertion functions to check default-config and
/// custom-chunk-config trees, immutable/mutable/range iterators, and key/value view
/// types at compile time.
///
/// 判定标准：所有断言都能通过编译；若任一 marker 缺失，本测试将编译失败。
/// Assertions: Every assertion compiles; if any marker is missing, this test fails to
/// compile.
#[test]
fn test_public_types_are_send_sync() {
    type SmallBPlus = BPlusTree<4, OrdComparer<i32>, i32, i32, SlabListConfig<CHUNK_SIZE_4K>>;
    type SmallBTree = BTree<4, OrdComparer<i32>, i32, i32, SlabListConfig<CHUNK_SIZE_4K>>;

    assert_send::<BPlusTree<4, OrdComparer<i32>, i32, i32>>();
    assert_sync::<BPlusTree<4, OrdComparer<i32>, i32, i32>>();
    assert_send::<BTree<4, OrdComparer<i32>, i32, i32>>();
    assert_sync::<BTree<4, OrdComparer<i32>, i32, i32>>();

    assert_send::<SmallBPlus>();
    assert_sync::<SmallBPlus>();
    assert_send::<SmallBTree>();
    assert_sync::<SmallBTree>();

    assert_send::<TreeIter<'static, 4, i32, i32>>();
    assert_sync::<TreeIter<'static, 4, i32, i32>>();
    assert_send::<RangeIter<'static, 4, i32, i32>>();
    assert_sync::<RangeIter<'static, 4, i32, i32>>();
    assert_send::<IterMut<'static, 4, i32, i32>>();
    assert_sync::<IterMut<'static, 4, i32, i32>>();
    assert_send::<RangeIterMut<'static, 4, i32, i32>>();
    assert_sync::<RangeIterMut<'static, 4, i32, i32>>();
    assert_send::<Keys<'static, 4, i32, i32>>();
    assert_sync::<Keys<'static, 4, i32, i32>>();
    assert_send::<Values<'static, 4, i32, i32>>();
    assert_sync::<Values<'static, 4, i32, i32>>();
    assert_send::<ValuesMut<'static, 4, i32, i32>>();
    assert_sync::<ValuesMut<'static, 4, i32, i32>>();

    assert_send::<BTreeIter<'static, 4, i32, i32>>();
    assert_sync::<BTreeIter<'static, 4, i32, i32>>();
    assert_send::<BTreeRangeIter<'static, 4, i32, i32>>();
    assert_sync::<BTreeRangeIter<'static, 4, i32, i32>>();
    assert_send::<BTreeIterMut<'static, 4, i32, i32>>();
    assert_sync::<BTreeIterMut<'static, 4, i32, i32>>();
    assert_send::<BTreeRangeIterMut<'static, 4, i32, i32>>();
    assert_sync::<BTreeRangeIterMut<'static, 4, i32, i32>>();
    assert_send::<BTreeKeys<'static, 4, i32, i32>>();
    assert_sync::<BTreeKeys<'static, 4, i32, i32>>();
    assert_send::<BTreeValues<'static, 4, i32, i32>>();
    assert_sync::<BTreeValues<'static, 4, i32, i32>>();
    assert_send::<BTreeValuesMut<'static, 4, i32, i32>>();
    assert_sync::<BTreeValuesMut<'static, 4, i32, i32>>();
}

/// 测试目标：验证 B+ Tree 可以移动到另一个线程，并可以跨线程共享只读访问。
/// Test goal: Verify that a B+ Tree can be moved to another thread and shared for
/// read-only access across threads.
///
/// 测试手段：构造一棵 B+ Tree 后在 `thread::scope` 中同时共享 `&tree` 做查询与
/// 迭代；另一个测试把树 `move` 进 `thread::spawn` 后查询。
/// Test method: Build a B+ Tree and share `&tree` across threads in `thread::scope`
/// for lookups and iteration; another case moves the tree into `thread::spawn` and
/// queries it there.
///
/// 判定标准：所有线程都能读到正确数据并正常结束，不出现编译错误或运行时 panic。
/// Assertions: Every thread reads the expected data and terminates normally without
/// compile errors or runtime panics.
#[test]
fn test_bplus_tree_cross_thread_usage() {
    let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    for key in 0..128 {
        tree.insert(key, key * 2);
    }

    thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                assert_eq!(tree.get(42), Some((&42, &84)));
                assert_eq!(tree.iter().count(), 128);
                assert_eq!(tree.range(10..20).count(), 10);
            });
        }
    });

    let sum = tree.iter().map(|(key, value)| *key + *value).sum::<i32>();
    let handle = thread::spawn(move || {
        assert_eq!(tree.get(100), Some((&100, &200)));
        tree.iter().map(|(key, value)| *key + *value).sum::<i32>()
    });
    assert_eq!(handle.join().unwrap(), sum);
}

/// 测试目标：验证 B Tree 可以移动到另一个线程，并可以跨线程共享只读访问。
/// Test goal: Verify that a B Tree can be moved to another thread and shared for
/// read-only access across threads.
///
/// 测试手段：构造一棵 B Tree 后在 `thread::scope` 中共享 `&tree` 做查询与迭代；
/// 随后把树 `move` 进 `thread::spawn` 后查询。
/// Test method: Build a B Tree and share `&tree` across threads in `thread::scope`
/// for lookups and iteration; then move the tree into `thread::spawn` and query it.
///
/// 判定标准：所有线程都能读到正确数据并正常结束。
/// Assertions: Every thread reads the expected data and terminates normally.
#[test]
fn test_btree_cross_thread_usage() {
    let mut tree = BTree::<4, _, i32, i32>::new().unwrap();
    for key in 0..128 {
        tree.insert(key, key * 3);
    }

    thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                assert_eq!(tree.get(42), Some((&42, &126)));
                assert_eq!(tree.iter().count(), 128);
                assert_eq!(tree.range(10..20).count(), 10);
            });
        }
    });

    let handle = thread::spawn(move || {
        assert_eq!(tree.get(100), Some((&100, &300)));
        tree.len()
    });
    assert_eq!(handle.join().unwrap(), 128);
}

/// 测试目标：验证不可变与可变迭代器可以跨线程移动并完成迭代。
/// Test goal: Verify that immutable and mutable iterators can be moved across threads
/// and complete their iteration.
///
/// 测试手段：在 `thread::scope` 中把 `tree.iter()` 移入子线程统计数量；再把
/// `tree.iter_mut()` 移入另一个子线程修改所有 value。
/// Test method: Move `tree.iter()` into a scoped thread to count items, then move
/// `tree.iter_mut()` into another scoped thread to mutate every value.
///
/// 判定标准：迭代器在另一个线程中正常结束；可变迭代完成后 value 已更新。
/// Assertions: The iterators finish normally on the other thread, and the values are
/// updated after the mutable iteration.
#[test]
fn test_iterators_can_move_across_threads() {
    let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    for key in 0..64 {
        tree.insert(key, key);
    }

    thread::scope(|scope| {
        let iter = tree.iter();
        scope.spawn(move || {
            assert_eq!(iter.count(), 64);
        });
    });

    thread::scope(|scope| {
        let iter = tree.iter_mut();
        scope.spawn(move || {
            for (_, value) in iter {
                *value += 1;
            }
        });
    });

    for key in 0..64 {
        assert_eq!(tree.get(key), Some((&key, &(key + 1))));
    }
}
