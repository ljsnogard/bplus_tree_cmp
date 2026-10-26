//! B+ Tree 公开 API 的集成测试。
//!
//! 这些测试只通过 `bplustree_cmp` 导出的类型和方法验证行为，不访问内部节点。

use core::{
    cmp::Ordering,
    ops::{Bound, RangeBounds},
};

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

/// 收集 B 树指定范围内的 key，便于与预期序列比较。
fn collect_range_<const M: usize, R>(
    tree: &BPlusTree<M, OrdComparer<i32>, i32, i32>,
    range: R,
) -> Vec<i32>
where
    R: RangeBounds<i32>,
{
    tree.range(range).map(|(key, _)| *key).collect()
}

/// 测试目标：验证 `range` 的闭开边界、无界边界、空区间与反向迭代。
/// Test goal: Verify `range` with inclusive/exclusive, unbounded, and empty bounds,
/// including reverse iteration.
///
/// 测试手段：在 `M = 2/3/4/6` 的 B 树中插入 0..64，逐一比较各类范围的结果。
/// Test method: Insert 0..64 into B trees with `M = 2/3/4/6` and compare the results
/// of many range shapes.
///
/// 判定标准：每个范围产生的 key 序列与预期一致；空区间不产生元素；`rev()` 产生
/// 严格降序的同集合。
/// Assertions: Each range yields the expected key sequence, empty ranges yield
/// nothing, and `rev()` yields the same set in strictly descending order.
#[test]
fn test_public_range_semantics() {
    for degree in [2, 3, 4, 6] {
        match degree {
            2 => range_semantics_for_::<2>(),
            3 => range_semantics_for_::<3>(),
            4 => range_semantics_for_::<4>(),
            6 => range_semantics_for_::<6>(),
            _ => unreachable!(),
        }
    }
}

#[allow(clippy::reversed_empty_ranges)] // 故意验证反向区间按 Comparer 语义为空
fn range_semantics_for_<const M: usize>() {
    let mut tree = BPlusTree::<M, _, i32, i32>::new().unwrap();
    for key in 0..64 {
        let _ = tree.insert(key, key * 10);
    }

    assert_eq!(collect_range_(&tree, ..).len(), 64);
    assert_eq!(collect_range_(&tree, 10..20), (10..20).collect::<Vec<_>>());
    assert_eq!(
        collect_range_(&tree, 10..=20),
        (10..=20).collect::<Vec<_>>()
    );
    assert_eq!(collect_range_(&tree, ..5), (0..5).collect::<Vec<_>>());
    assert_eq!(collect_range_(&tree, ..=5), (0..=5).collect::<Vec<_>>());
    assert_eq!(collect_range_(&tree, 60..), (60..64).collect::<Vec<_>>());
    assert_eq!(
        collect_range_(&tree, (Bound::Excluded(10), Bound::Included(13))),
        vec![11, 12, 13]
    );
    assert_eq!(
        collect_range_(&tree, (Bound::Excluded(10), Bound::Excluded(13))),
        vec![11, 12]
    );

    assert!(collect_range_(&tree, 10..10).is_empty());
    assert!(collect_range_(&tree, 20..10).is_empty());
    assert!(collect_range_(&tree, 100..200).is_empty());
    assert!(collect_range_(&tree, (Bound::Excluded(10), Bound::Included(10))).is_empty());

    let reversed: Vec<i32> = tree.range(10..20).rev().map(|(key, _)| *key).collect();
    assert_eq!(reversed, (10..20).rev().collect::<Vec<_>>());

    let forward: Vec<i32> = tree.range(10..20).map(|(key, _)| *key).collect();
    let mut mixed = Vec::new();
    let mut iter = tree.range(10..20);
    mixed.push(*iter.next().unwrap().0);
    mixed.push(*iter.next_back().unwrap().0);
    mixed.extend(iter.map(|(key, _)| *key));
    assert_eq!(mixed, {
        let mut expected = forward;
        let last = expected.pop().unwrap();
        let first = expected.remove(0);
        let mut sequence = vec![first, last];
        sequence.extend(expected);
        sequence
    });
}

/// 测试目标：验证 `range_mut`、`iter_mut` 与 `values_mut` 只修改范围内的 value。
/// Test goal: Verify that `range_mut`, `iter_mut`, and `values_mut` mutate only the
/// intended values.
///
/// 测试手段：插入 0..32，使用 `range_mut(8..16)` 修改 value；同时从同一个根叶子中
/// 连续取出两个可变引用，验证它们可以同时存活并被写入。
/// Test method: Insert 0..32, mutate through `range_mut(8..16)`, and take two mutable
/// references from the same root leaf at once to verify they can coexist.
///
/// 判定标准：范围内 value 全部加 100，范围外保持不变；同一节点中的两个 `&mut V`
/// 都能正确写回。
/// Assertions: In-range values gain 100 while out-of-range values stay unchanged, and
/// two `&mut V` references into one node both write through correctly.
#[test]
fn test_public_range_mut_and_iter_mut() {
    let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    for key in 0..3 {
        let _ = tree.insert(key, key * 10);
    }

    {
        let mut iter = tree.iter_mut();
        let (_, first) = iter.next().expect("应至少有一项");
        let (_, second) = iter.next().expect("应至少有两项");
        *first += 1;
        *second += 2;
    }
    assert_eq!(tree.get(0), Some((&0, &1)));
    assert_eq!(tree.get(1), Some((&1, &12)));
    assert_eq!(tree.get(2), Some((&2, &20)));

    for (_, value) in tree.range_mut(1..3) {
        *value += 100;
    }
    assert_eq!(tree.get(0), Some((&0, &1)));
    assert_eq!(tree.get(1), Some((&1, &112)));
    assert_eq!(tree.get(2), Some((&2, &120)));

    for value in tree.values_mut() {
        *value += 1;
    }
    assert_eq!(tree.get(0), Some((&0, &2)));
    assert_eq!(tree.get(2), Some((&2, &121)));
}

/// 测试目标：验证 `range` 的 key/value 视图以及 `keys`、`values` 的顺序。
/// Test goal: Verify the key/value views of ranges and the ordering of `keys` and
/// `values`.
///
/// 测试手段：插入固定集合后分别收集 `keys`、`values`、`range(..).rev()` 的结果。
/// Test method: Insert a fixed set and collect `keys`, `values`, and a reversed range.
///
/// 判定标准：key 升序、value 与 key 的映射一致，反向范围严格降序。
/// Assertions: Keys are ascending, values match their keys, and the reversed range is
/// strictly descending.
#[test]
fn test_public_keys_values_and_order() {
    let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    for key in [3, 1, 4, 2] {
        let _ = tree.insert(key, key * 10);
    }

    assert_eq!(tree.keys().copied().collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    assert_eq!(
        tree.values().copied().collect::<Vec<_>>(),
        vec![10, 20, 30, 40]
    );
    assert_eq!(
        tree.range(1..4)
            .rev()
            .map(|(key, _)| *key)
            .collect::<Vec<_>>(),
        vec![3, 2, 1]
    );
    assert_eq!(tree.first_key_value(), Some((&1, &10)));
    assert_eq!(tree.last_key_value(), Some((&4, &40)));
    assert!(tree.contains_key(3));
    assert!(!tree.contains_key(5));

    tree.clear();
    assert!(tree.is_empty());
    assert_eq!(tree.range(..).count(), 0);
}

/// 测试目标：验证异构 Comparer 的范围查询不需要构造 Key。
/// Test goal: Verify that heterogeneous range queries do not require constructing a
/// `K`.
///
/// 测试手段：树中保存按 `User.id` 排序的 `User`，用 `u64` 范围与
/// `TrComparer<User, u64>` 查询；另用 `&str` 范围查询 `String` 键。
/// Test method: Store `User` ordered by `User.id` and query with `u64` ranges plus
/// `TrComparer<User, u64>`; additionally query `String` keys with `&str` ranges.
///
/// 判定标准：结果只包含范围内 id，且不产生任何临时 `User`。
/// Assertions: Results contain exactly the in-range ids and require no temporary
/// `User`.
#[test]
fn test_public_heterogeneous_range() {
    let mut tree = BPlusTree::<4, _, User, u64>::with_comparer(UserIdCmp).expect("配置应当有效");
    for id in 0..32u64 {
        assert!(
            tree.try_insert(User { id, name: "member" }, id * 2,)
                .is_ok()
        );
    }

    let cmp = UserIdQueryCmp;
    let ids: Vec<u64> = tree
        .range_with_comparer(5u64..10u64, &cmp)
        .map(|(user, _)| user.id)
        .collect();
    assert_eq!(ids, (5..10).collect::<Vec<_>>());

    for (_, value) in tree.range_mut_with_comparer(5u64..10u64, &cmp) {
        *value += 1;
    }
    assert_eq!(
        tree.get_with_comparer(&7u64, &cmp).map(|(_, value)| *value),
        Some(15)
    );

    let mut tree = BPlusTree::<4, _, String, i32>::new().unwrap();
    for (key, value) in [("a", 1), ("b", 2), ("c", 3), ("d", 4)] {
        let _ = tree.insert(key.to_string(), value);
    }
    let cmp = |a: &String, b: &&str| a.as_str().cmp(*b);
    let keys: Vec<&str> = tree
        .range_with_comparer("b".."d", &cmp)
        .map(|(key, _)| key.as_str())
        .collect();
    assert_eq!(keys, vec!["b", "c"]);
}

struct ReverseOrder;

impl TrComparer<i32> for ReverseOrder {
    fn compare(&self, a: &i32, b: &i32) -> Ordering {
        b.cmp(a)
    }
}

/// 测试目标：验证范围方向完全跟随 Comparer，而不是 `i32` 的自然顺序。
/// Test goal: Verify that range direction follows the comparer rather than the
/// natural order of `i32`.
///
/// 测试手段：使用反向 Comparer 插入 0..32，并执行 `range(20..10)` 与可变范围。
/// Test method: Insert 0..32 with a reverse comparer and run `range(20..10)` plus a
/// mutable range.
///
/// 判定标准：范围按 Comparer 顺序从 20 递减到 11；可变范围只改对应元素。
/// Assertions: The range yields 20 down to 11 in comparer order, and the mutable range
/// changes exactly those elements.
#[test]
#[allow(clippy::reversed_empty_ranges)] // 反面 Comparer 下 20..10 是有效区间
fn test_public_reverse_comparer_range() {
    let mut tree = BPlusTree::<4, _, i32, i32>::with_comparer(ReverseOrder).unwrap();
    for key in 0..32 {
        let _ = tree.insert(key, key);
    }

    let keys: Vec<i32> = tree.range(20..10).map(|(key, _)| *key).collect();
    assert_eq!(keys, (11..=20).rev().collect::<Vec<_>>());

    for (_, value) in tree.range_mut(20..10) {
        *value += 100;
    }
    for key in 0..32 {
        let expected = if (11..=20).contains(&key) {
            key + 100
        } else {
            key
        };
        assert_eq!(tree.get(key), Some((&key, &expected)));
    }
}

/// 测试目标：验证 `&mut BTree` 的 `IntoIterator` 实现。
/// Test goal: Verify the `IntoIterator` implementation for `&mut BTree`.
///
/// 测试手段：通过 `for (key, value) in &mut tree` 修改全部 value。
/// Test method: Mutate every value through `for (key, value) in &mut tree`.
///
/// 判定标准：所有 value 都按预期更新，key 保持不变。
/// Assertions: Every value is updated as expected and keys stay unchanged.
#[test]
fn test_public_into_iter_mut() {
    let mut tree = BPlusTree::<4, _, i32, i32>::new().unwrap();
    for key in 0..16 {
        let _ = tree.insert(key, key);
    }

    for (key, value) in &mut tree {
        *value = *key * 3;
    }
    for key in 0..16 {
        assert_eq!(tree.get(key), Some((&key, &(key * 3))));
    }
}

/// 测试目标：用随机边界与 `BTreeMap::range` 对拍 B+ Tree 的范围语义。
/// Test goal: Differential-test B+ Tree range semantics against `BTreeMap::range` with
/// random bounds.
///
/// 测试手段：在 `M = 2/3/4/6` 的树中插入 0..256、删除部分 key，然后用固定种子的
/// xorshift 生成 400 组随机 `(开始, 结束, 开闭)` 边界（Miri 下缩减为 30 组）；每组比较正向、反向结果，
/// 并周期性通过 `range_mut` 同步修改树与模型。
/// Test method: Insert 0..256 into trees with `M = 2/3/4/6`, remove some keys, then use
/// a fixed-seed xorshift to generate 400 random `(start, end, inclusivity)` bounds.
/// Compare forward and reverse results, and periodically mutate both the tree and the
/// model through `range_mut`.
///
/// 判定标准：正向/反向结果与模型完全一致；同步可变修改后完整迭代仍然一致。
/// Assertions: Forward and reverse results match the model exactly, and full iteration
/// still matches after synchronized mutable updates.
#[test]
fn test_public_random_ranges_against_btreemap() {
    for degree in [2, 3, 4, 6] {
        match degree {
            2 => random_ranges_for_::<2>(),
            3 => random_ranges_for_::<3>(),
            4 => random_ranges_for_::<4>(),
            6 => random_ranges_for_::<6>(),
            _ => unreachable!(),
        }
    }
}

fn random_ranges_for_<const M: usize>() {
    use std::collections::BTreeMap;

    let mut tree = BPlusTree::<M, _, i32, i32>::new().unwrap();
    let mut model = BTreeMap::<i32, i32>::new();
    for key in 0..256 {
        let _ = tree.insert(key, key * 3);
        let _ = model.insert(key, key * 3);
    }
    for key in (0..256).step_by(11) {
        assert_eq!(tree.remove(key), model.remove(&key));
    }

    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let steps = if cfg!(miri) { 30 } else { 400 };
    for step in 0..steps {
        let a = (next_rand_(&mut state) % 300) as i32;
        let b = (next_rand_(&mut state) % 300) as i32;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let start_inclusive = next_rand_(&mut state) % 2 == 0;
        let end_inclusive = next_rand_(&mut state) % 2 == 0;
        if start == end && !start_inclusive && !end_inclusive {
            continue;
        }

        let start_bound = if start_inclusive {
            Bound::Included(start)
        } else {
            Bound::Excluded(start)
        };
        let end_bound = if end_inclusive {
            Bound::Included(end)
        } else {
            Bound::Excluded(end)
        };

        let actual: Vec<(i32, i32)> = tree
            .range((start_bound, end_bound))
            .map(|(key, value)| (*key, *value))
            .collect();
        let expected: Vec<(i32, i32)> = model
            .range((start_bound, end_bound))
            .map(|(key, value)| (*key, *value))
            .collect();
        assert_eq!(
            actual, expected,
            "forward range {start_bound:?}..{end_bound:?}"
        );

        let actual_rev: Vec<(i32, i32)> = tree
            .range((start_bound, end_bound))
            .rev()
            .map(|(key, value)| (*key, *value))
            .collect();
        let expected_rev: Vec<(i32, i32)> = model
            .range((start_bound, end_bound))
            .rev()
            .map(|(key, value)| (*key, *value))
            .collect();
        assert_eq!(
            actual_rev, expected_rev,
            "reverse range {start_bound:?}..{end_bound:?}"
        );

        if step % 23 == 0 {
            for (_, value) in tree.range_mut((start_bound, end_bound)) {
                *value += 7;
            }
            for (_, value) in model.range_mut((start_bound, end_bound)) {
                *value += 7;
            }
        }
    }

    let actual: Vec<(i32, i32)> = tree.iter().map(|(key, value)| (*key, *value)).collect();
    let expected: Vec<(i32, i32)> = model.iter().map(|(key, value)| (*key, *value)).collect();
    assert_eq!(actual, expected);
}

fn next_rand_(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// 测试目标：验证 `range_mut` 可以同时向调用者交出大量可变引用。
/// Test goal: Verify that `range_mut` may hand out many mutable references that stay
/// alive at the same time.
///
/// 测试手段：在 `M = 2` 的 B 树中插入 64 个元素，用 `range_mut(..)` 收集全部
/// `(&K, &mut V)`（跨越多个叶子与内部节点），然后逐个写入。
/// Test method: Insert 64 entries into an `M = 2` B+ Tree, collect all `(&K, &mut V)`
/// from `range_mut(..)` (spanning several leaves and internal nodes), then write
/// through each one.
///
/// 判定标准：所有写入都生效，树内容与预期一致；该测试同时用于 Miri 检查字段级
/// 裸指针访问没有违反别名规则。
/// Assertions: Every write takes effect and the tree matches expectations; the test
/// also exercises Miri checking that the field-level raw pointer access respects
/// aliasing rules.
#[test]
fn test_public_range_mut_holds_many_refs() {
    let mut tree = BPlusTree::<2, _, i32, i32>::new().unwrap();
    for key in 0..64 {
        let _ = tree.insert(key, key);
    }

    let refs: Vec<(&i32, &mut i32)> = tree.range_mut(..).collect();
    assert_eq!(refs.len(), 64);
    for (key, value) in refs {
        *value = *key * 5;
    }
    for key in 0..64 {
        assert_eq!(tree.get(key), Some((&key, &(key * 5))));
    }
}
