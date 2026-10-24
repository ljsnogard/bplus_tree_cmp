//! B+ Tree、B Tree 与 `std::collections::BTreeMap` 的常见操作基准。
//!
//! 覆盖有序映射最常用的六类操作：
//!
//! - `insert`：插入全部 key；
//! - `lookup hit`：查找全部已存在的 key；
//! - `lookup miss`：查找全部不存在的 key；
//! - `update`：对全部已存在的 key 通过 `get_mut` 修改 value；
//! - `iterate`：按 key 升序遍历全部元素并累加校验和；
//! - `remove`：删除全部 key。
//!
//! 运行：
//!
//! ```text
//! cargo bench --bench btreemap_compare
//! BENCH_N=500000 BENCH_SAMPLES=7 cargo bench --bench btreemap_compare
//! ```
//!
//! 所有容器使用完全相同的随机 key、插入顺序、查询序列与删除顺序。
//! 表中数值是多次采样的中位数，单位为 ns/op（每次操作纳秒数），越小越好。
//! 构造/重建容器的时间不计入 lookup、update、iterate、remove 的操作耗时，
//! 但计入 insert 的耗时。
//!
//! 注意：两棵树的节点与数据都来自 `SlabList`。B+ Tree 的插入/删除沿父指针
//! 维护 separator，不会产生逐操作的 `Vec` 堆分配；但仍包含 SlabList 的槽位
//! 分配/回收成本。

use std::{
    collections::BTreeMap,
    env,
    hint::black_box,
    time::{Duration, Instant},
};

use bplustree_cmp::{BPlusTree, BTree, OrdComparer};

const DEFAULT_N: usize = 100_000;
const DEFAULT_SAMPLES: usize = 5;

/// 简单的确定性 xorshift，保证不同运行之间数据一致。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// 三类容器共用的基准操作抽象。
trait BenchMap {
    /// 创建一个空容器。
    fn empty() -> Self;

    /// 插入或覆盖一个键值对。
    fn insert_value(&mut self, key: u64, value: u64);

    /// 查找并复制 value。
    fn get_value(&self, key: u64) -> Option<u64>;

    /// 查找并返回 value 的可变引用。
    fn get_mut_value(&mut self, key: u64) -> Option<&mut u64>;

    /// 删除 key，返回旧 value。
    fn remove_key(&mut self, key: u64) -> Option<u64>;

    /// 按 key 升序遍历并累加所有 key/value。
    fn checksum(&self) -> u64;
}

impl BenchMap for BTreeMap<u64, u64> {
    fn empty() -> Self {
        BTreeMap::new()
    }

    fn insert_value(&mut self, key: u64, value: u64) {
        let _ = self.insert(key, value);
    }

    fn get_value(&self, key: u64) -> Option<u64> {
        self.get(&key).copied()
    }

    fn get_mut_value(&mut self, key: u64) -> Option<&mut u64> {
        self.get_mut(&key)
    }

    fn remove_key(&mut self, key: u64) -> Option<u64> {
        self.remove(&key)
    }

    fn checksum(&self) -> u64 {
        self.iter().fold(0u64, |sum, (&key, &value)| {
            sum.wrapping_add(key).wrapping_add(value)
        })
    }
}

impl<const M: usize> BenchMap for BPlusTree<M, OrdComparer<u64>, u64, u64> {
    fn empty() -> Self {
        BPlusTree::<M, OrdComparer<u64>, u64, u64>::new().expect("默认 SlabList 配置应当有效")
    }

    fn insert_value(&mut self, key: u64, value: u64) {
        let _ = BPlusTree::<M, OrdComparer<u64>, u64, u64>::insert(self, key, value);
    }

    fn get_value(&self, key: u64) -> Option<u64> {
        BPlusTree::<M, OrdComparer<u64>, u64, u64>::get(self, key).map(|(_, value)| *value)
    }

    fn get_mut_value(&mut self, key: u64) -> Option<&mut u64> {
        BPlusTree::<M, OrdComparer<u64>, u64, u64>::get_mut(self, key).map(|(_, value)| value)
    }

    fn remove_key(&mut self, key: u64) -> Option<u64> {
        BPlusTree::<M, OrdComparer<u64>, u64, u64>::remove(self, key)
    }

    fn checksum(&self) -> u64 {
        self.iter().fold(0u64, |sum, (key, value)| {
            sum.wrapping_add(*key).wrapping_add(*value)
        })
    }
}

impl<const M: usize> BenchMap for BTree<M, OrdComparer<u64>, u64, u64> {
    fn empty() -> Self {
        BTree::<M, OrdComparer<u64>, u64, u64>::new().expect("默认 SlabList 配置应当有效")
    }

    fn insert_value(&mut self, key: u64, value: u64) {
        let _ = BTree::<M, OrdComparer<u64>, u64, u64>::insert(self, key, value);
    }

    fn get_value(&self, key: u64) -> Option<u64> {
        BTree::<M, OrdComparer<u64>, u64, u64>::get(self, key).map(|(_, value)| *value)
    }

    fn get_mut_value(&mut self, key: u64) -> Option<&mut u64> {
        BTree::<M, OrdComparer<u64>, u64, u64>::get_mut(self, key).map(|(_, value)| value)
    }

    fn remove_key(&mut self, key: u64) -> Option<u64> {
        BTree::<M, OrdComparer<u64>, u64, u64>::remove(self, key)
    }

    fn checksum(&self) -> u64 {
        self.iter().fold(0u64, |sum, (key, value)| {
            sum.wrapping_add(*key).wrapping_add(*value)
        })
    }
}

#[derive(Clone, Copy)]
struct Metrics {
    insert: f64,
    lookup_hit: f64,
    lookup_miss: f64,
    update: f64,
    iterate: f64,
    remove: f64,
}

type MetricSelector = fn(&Metrics) -> f64;

const METRIC_ROWS: [(&str, MetricSelector); 6] = [
    ("insert", |m| m.insert),
    ("lookup hit", |m| m.lookup_hit),
    ("lookup miss", |m| m.lookup_miss),
    ("update", |m| m.update),
    ("iterate", |m| m.iterate),
    ("remove", |m| m.remove),
];

fn main() {
    let n = env_usize("BENCH_N", DEFAULT_N).max(1);
    let samples = env_usize("BENCH_SAMPLES", DEFAULT_SAMPLES).max(1);

    let mut rng = Rng(0x1234_5678_9abc_def0);
    let mut keys: Vec<u64> = (0..n as u64).collect();
    shuffle(&mut keys, &mut rng);

    let ops: Vec<(u64, u64)> = keys
        .iter()
        .map(|&key| (key, key.wrapping_mul(0x9e37_79b9_7f4a_7c15)))
        .collect();

    let mut hits = keys.clone();
    shuffle(&mut hits, &mut rng);
    let mut misses: Vec<u64> = (n as u64..(n as u64).saturating_mul(2)).collect();
    shuffle(&mut misses, &mut rng);

    println!("BPlusTreeCmp benchmark");
    println!("N = {n}, samples = {samples}, value type = u64 -> u64, ns/op = median");
    println!("keys are shuffled before insertion and lookup");
    println!();

    let columns = [
        (
            "B+Tree M=4",
            bench_bplus::<4>(&ops, &hits, &misses, samples),
        ),
        (
            "B+Tree M=16",
            bench_bplus::<16>(&ops, &hits, &misses, samples),
        ),
        ("BTree M=4", bench_btree::<4>(&ops, &hits, &misses, samples)),
        (
            "BTree M=16",
            bench_btree::<16>(&ops, &hits, &misses, samples),
        ),
        (
            "BTreeMap",
            bench_map::<BTreeMap<u64, u64>>(&ops, &hits, &misses, samples),
        ),
    ];

    println!("absolute cost (ns/op, lower is better)");
    print_table(&columns);
    println!();
    println!("ratio vs BTreeMap (lower is better)");
    print_ratios(&columns);
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn shuffle<T>(values: &mut [T], rng: &mut Rng) {
    for i in (1..values.len()).rev() {
        let j = (rng.next() as usize) % (i + 1);
        values.swap(i, j);
    }
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn per_op_ns(duration: Duration, n: usize) -> f64 {
    duration.as_secs_f64() * 1e9 / n as f64
}

fn bench_bplus<const M: usize>(
    ops: &[(u64, u64)],
    hits: &[u64],
    misses: &[u64],
    samples: usize,
) -> Metrics {
    bench_map::<BPlusTree<M, OrdComparer<u64>, u64, u64>>(ops, hits, misses, samples)
}

fn bench_btree<const M: usize>(
    ops: &[(u64, u64)],
    hits: &[u64],
    misses: &[u64],
    samples: usize,
) -> Metrics {
    bench_map::<BTree<M, OrdComparer<u64>, u64, u64>>(ops, hits, misses, samples)
}

fn bench_map<T: BenchMap>(
    ops: &[(u64, u64)],
    hits: &[u64],
    misses: &[u64],
    samples: usize,
) -> Metrics {
    let build = || {
        let mut map = T::empty();
        for &(key, value) in ops {
            map.insert_value(key, value);
        }
        map
    };

    let mut insert_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let mut map = T::empty();
        for &(key, value) in ops {
            map.insert_value(key, value);
        }
        let elapsed = start.elapsed();
        black_box(&map);
        drop(map);
        insert_samples.push(elapsed);
    }

    let map = build();

    let mut hit_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let mut checksum = 0u64;
        for &key in hits {
            if let Some(value) = map.get_value(key) {
                checksum = checksum.wrapping_add(value);
            }
        }
        hit_samples.push(start.elapsed());
        black_box(checksum);
    }

    let mut miss_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let mut checksum = 0u64;
        for &key in misses {
            if let Some(value) = map.get_value(key) {
                checksum = checksum.wrapping_add(value);
            }
        }
        miss_samples.push(start.elapsed());
        black_box(checksum);
    }

    let mut update_samples = Vec::with_capacity(samples);
    let mut updated_map = build();
    for _ in 0..samples {
        let start = Instant::now();
        let mut checksum = 0u64;
        for &key in hits {
            if let Some(value) = updated_map.get_mut_value(key) {
                *value = value.wrapping_add(1);
                checksum = checksum.wrapping_add(*value);
            }
        }
        update_samples.push(start.elapsed());
        black_box(checksum);
    }

    let mut iterate_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let checksum = map.checksum();
        iterate_samples.push(start.elapsed());
        black_box(checksum);
    }

    let mut remove_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let mut map = build();
        let start = Instant::now();
        for &(key, _) in ops {
            black_box(map.remove_key(key));
        }
        let elapsed = start.elapsed();
        black_box(&map);
        drop(map);
        remove_samples.push(elapsed);
    }

    let n = ops.len().max(1);
    Metrics {
        insert: per_op_ns(median(insert_samples), n),
        lookup_hit: per_op_ns(median(hit_samples), n),
        lookup_miss: per_op_ns(median(miss_samples), n),
        update: per_op_ns(median(update_samples), n),
        iterate: per_op_ns(median(iterate_samples), n),
        remove: per_op_ns(median(remove_samples), n),
    }
}

fn print_table(columns: &[(&str, Metrics)]) {
    print!("{:<14}", "operation");
    for (label, _) in columns {
        print!("{label:>16}");
    }
    println!();

    for (name, select) in METRIC_ROWS {
        print!("{name:<14}");
        for (_, metrics) in columns {
            print!("{:>16.2}", select(metrics));
        }
        println!();
    }
}

fn print_ratios(columns: &[(&str, Metrics)]) {
    let std_metrics = columns.last().expect("至少应有一个基准列").1;

    print!("{:<14}", "operation");
    for (label, _) in &columns[..columns.len() - 1] {
        print!("{label:>16}");
    }
    println!();

    for (name, select) in METRIC_ROWS {
        print!("{name:<14}");
        for (_, metrics) in &columns[..columns.len() - 1] {
            print!("{:>15.2}x", select(metrics) / select(&std_metrics));
        }
        println!();
    }
}
