//! B+ Tree 公开 API 的集成测试。
//!
//! 这些测试只通过 `bplustree_cmp` 导出的类型和方法验证行为，不访问内部节点。

use core::cmp::Ordering;

use bplustree_cmp::{BPlusTree, ConflictInfo, OrdComparer, TrComparer, TreeIter};

/// 测试目标：验证公开 API 的插入、查询、首尾、迭代和删除。
/// Test goal: Verify insertion, lookup, first/last, iteration, and removal through
/// the public API.
///
/// 测试手段：用 `OrdComparer` 创建 `u64 -> &str` 树，插入乱序数据，使用 `get`、
/// `first`、`last`、`iter`、`&tree` 迭代和 `remove`。
/// Test method: Create a `u64 -> &str` tree with `OrdComparer`, insert out-of-order
/// entries, and use `get`, `first`, `last`, `iter`, `&tree` iteration, and `remove`.
///
/// 判定标准：查询值正确，首尾为最小/最大 key，迭代严格升序，删除返回旧 value
/// 且之后查询为 `None`。
/// Assertions: Lookups return the expected values, first/last are the minimum and
/// maximum keys, iteration is ascending, and removal returns the old value and makes
/// the key unreachable.
#[test]
fn test_public_basic_operations() {
    let mut tree = BPlusTree::<4, _, u64, &'static str>::new().expect("配置应当有效");

    let entries = [
        (5u64, "five"),
        (1, "one"),
        (3, "three"),
        (2, "two"),
        (4, "four"),
    ];
    for (key, value) in entries {
        assert_eq!(tree.insert(key, value), None);
    }

    assert_eq!(tree.len(), 5);
    assert!(!tree.is_empty());
    assert_eq!(tree.first(), Some((&1, &"one")));
    assert_eq!(tree.last(), Some((&5, &"five")));
    assert_eq!(tree.get(3), Some((&3, &"three")));

    let conflict: ConflictInfo<'_, u64, &'static str> = tree.try_insert(5, "cinco").unwrap_err();
    assert_eq!(conflict.existing(), (&5, &"five"));
    assert_eq!(conflict.conflict(), Some(&(5, "cinco")));

    let iter: TreeIter<'_, 4, u64, &'static str> = tree.iter();
    assert_eq!(iter.len(), 5);
    let by_iter: Vec<(u64, &str)> = tree.iter().map(|(key, value)| (*key, *value)).collect();
    assert_eq!(
        by_iter,
        vec![
            (1, "one"),
            (2, "two"),
            (3, "three"),
            (4, "four"),
            (5, "five")
        ]
    );

    let by_ref: Vec<u64> = (&tree).into_iter().map(|(key, _)| *key).collect();
    assert_eq!(by_ref, vec![1, 2, 3, 4, 5]);

    assert_eq!(tree.remove(3), Some("three"));
    assert_eq!(tree.get(3), None);
    assert_eq!(tree.remove(3), None);
    assert_eq!(tree.len(), 4);
}

#[derive(Debug)]
struct User {
    id: u64,
    name: &'static str,
}

struct UserIdCmp;

impl TrComparer<User> for UserIdCmp {
    fn compare(&self, a: &User, b: &User) -> Ordering {
        a.id.cmp(&b.id)
    }
}

struct UserIdQueryCmp;

impl TrComparer<User, u64> for UserIdQueryCmp {
    fn compare(&self, a: &User, b: &u64) -> Ordering {
        a.id.cmp(b)
    }
}

/// 测试目标：验证公开 API 的异构查询与按查询类型删除。
/// Test goal: Verify heterogeneous lookup and removal by a query type through the
/// public API.
///
/// 测试手段：树中保存按 `User.id` 排序的 `User`，用 `u64` query 与
/// `TrComparer<User, u64>` 插入、查询、删除。
/// Test method: Store `User` ordered by `User.id`, then insert, query, and remove
/// using a `u64` query and `TrComparer<User, u64>`.
///
/// 判定标准：不需要构造临时 `User` 即可按 id 查到或删除正确的 User。
/// Assertions: The correct user can be looked up or removed by id without
/// constructing a temporary `User`.
#[test]
fn test_public_heterogeneous_query() {
    let mut tree = BPlusTree::<4, _, User, u64>::with_comparer(UserIdCmp).expect("配置应当有效");
    let cmp = UserIdQueryCmp;

    for id in 0..32u64 {
        let result = tree.try_insert_with_comparer(&id, &cmp, |id| {
            (
                User {
                    id: *id,
                    name: "member",
                },
                *id * 5,
            )
        });
        assert!(result.is_ok());
    }

    let (user, value) = tree
        .get_with_comparer(&17u64, &cmp)
        .expect("按 id 应能查到");
    assert_eq!(user.id, 17);
    assert_eq!(user.name, "member");
    assert_eq!(*value, 85);

    let (user, value) = tree
        .remove_entry_with_comparer(&17u64, &cmp)
        .expect("按 id 应能删除");
    assert_eq!(user.id, 17);
    assert_eq!(value, 85);
    assert!(tree.get_with_comparer(&17u64, &cmp).is_none());
}

/// 测试目标：验证从已有集合构建、`collect` 与重复 key 的保留语义。
/// Test goal: Verify construction from an existing collection, `collect`, and the
/// duplicate-key retention semantics.
///
/// 测试手段：分别使用 `from_iter_with_comparer` 和 `FromIterator::from_iter` 构建含重复 key
/// 的树。
/// Test method: Build trees containing duplicate keys with `from_iter_with_comparer` and
/// `FromIterator::from_iter`.
///
/// 判定标准：重复 key 保留最后一个 value，长度和有序迭代正确。
/// Assertions: Duplicate keys keep the last value, and the length and ordered
/// iteration are correct.
#[test]
fn test_public_build_from_collection() {
    let items = vec![(3, "three"), (1, "one"), (2, "two"), (1, "uno")];

    let mut from_fn = BPlusTree::<4, _, i32, &'static str>::from_iter_with_comparer(
        OrdComparer::new(),
        items.clone(),
    )
    .expect("配置应当有效");
    assert_eq!(from_fn.len(), 3);
    assert_eq!(from_fn.get(1), Some((&1, &"uno")));

    let collected: BPlusTree<4, OrdComparer<i32>, i32, &'static str> = items.into_iter().collect();
    assert_eq!(collected.len(), 3);
    assert_eq!(collected.get(1), Some((&1, &"uno")));

    from_fn.extend(vec![(4, "four"), (0, "zero")]);
    let keys: Vec<i32> = from_fn.iter().map(|(key, _)| *key).collect();
    assert_eq!(keys, vec![0, 1, 2, 3, 4]);
}
