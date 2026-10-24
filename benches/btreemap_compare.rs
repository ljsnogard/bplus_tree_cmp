//! Comparer-first B+ Tree 与 `std::collections::BTreeMap` 的性能对比基准。
//!
//! 运行：
//!
//! ```text
//! cargo bench --bench btreemap_compare
//! BENCH_N=500000 BENCH_SAMPLES=7 cargo bench --bench btreemap_compare
//! ```
//!
//! 两种容器使用完全相同的随机 key 插入顺序、查询序列和删除顺序。
//! 表中数值是多次采样的中位数，单位为 ns/op（每次操作纳秒数），越小越好。
//!
//! 注意：`BPlusTree` 的插入/删除路径由独立的 `SlabList<PathFrame>` 分配，
//! 操作结束后归还帧；它不会产生每次操作的 `Vec` 堆分配，但仍包含
//! SlabList 的槽位分配/回收成本。

use std::{
    collections::BTreeMap,
    env,
    hint::black_box,
    time::{Duration, Instant},
};

use bplustree_cmp::{BPlusTree, OrdComparer};

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

#[derive(Clone, Copy)]
struct Metrics {
    insert: f64,
    lookup_hit: f64,
    lookup_miss: f64,
    iterate: f64,
    remove: f64,
}

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

    let m4 = bench_bplus::<4>(&ops, &hits, &misses, samples);
    let m16 = bench_bplus::<16>(&ops, &hits, &misses, samples);
    let btree = bench_btreemap(&ops, &hits, &misses, samples);

    println!(
        "{:<14}{:>16}{:>17}{:>16}",
        "operation", "B+Tree(M=4)", "B+Tree(M=16)", "BTreeMap"
    );
    print_row("insert", &m4, &m16, &btree, |m| m.insert);
    print_row("lookup hit", &m4, &m16, &btree, |m| m.lookup_hit);
    print_row("lookup miss", &m4, &m16, &btree, |m| m.lookup_miss);
    print_row("iterate", &m4, &m16, &btree, |m| m.iterate);
    print_row("remove", &m4, &m16, &btree, |m| m.remove);
    println!();

    println!("ratio vs BTreeMap (lower is better)");
    println!(
        "{:<14}{:>16}{:>17}",
        "operation", "B+Tree(M=4)", "B+Tree(M=16)"
    );
    print_ratio("insert", &m4, &m16, &btree, |m| m.insert);
    print_ratio("lookup hit", &m4, &m16, &btree, |m| m.lookup_hit);
    print_ratio("lookup miss", &m4, &m16, &btree, |m| m.lookup_miss);
    print_ratio("iterate", &m4, &m16, &btree, |m| m.iterate);
    print_ratio("remove", &m4, &m16, &btree, |m| m.remove);
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
    type Tree<const M: usize> = BPlusTree<M, OrdComparer<u64>, u64, u64>;

    let build = || {
        let mut tree = Tree::<M>::new_in(OrdComparer::new()).expect("默认配置应当有效");
        for &(key, value) in ops {
            let _ = tree.insert(key, value);
        }
        tree
    };

    let mut insert_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let mut tree = Tree::<M>::new_in(OrdComparer::new()).expect("默认配置应当有效");
        for &(key, value) in ops {
            black_box(tree.insert(key, value));
        }
        insert_samples.push(start.elapsed());
        black_box(&tree);
        drop(tree);
    }

    let tree = build();

    let mut hit_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let mut checksum = 0u64;
        for &key in hits {
            if let Some((_, value)) = tree.get(key) {
                checksum = checksum.wrapping_add(*value);
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
            if let Some((_, value)) = tree.get(key) {
                checksum = checksum.wrapping_add(*value);
            }
        }
        miss_samples.push(start.elapsed());
        black_box(checksum);
    }

    let mut iterate_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let mut checksum = 0u64;
        for (key, value) in &tree {
            checksum = checksum.wrapping_add(*key).wrapping_add(*value);
        }
        iterate_samples.push(start.elapsed());
        black_box(checksum);
    }

    let mut remove_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let mut tree = build();
        let start = Instant::now();
        for &(key, _) in ops {
            black_box(tree.remove(key));
        }
        remove_samples.push(start.elapsed());
        black_box(&tree);
        drop(tree);
    }

    let n = ops.len().max(1);
    Metrics {
        insert: per_op_ns(median(insert_samples), n),
        lookup_hit: per_op_ns(median(hit_samples), n),
        lookup_miss: per_op_ns(median(miss_samples), n),
        iterate: per_op_ns(median(iterate_samples), n),
        remove: per_op_ns(median(remove_samples), n),
    }
}

fn bench_btreemap(ops: &[(u64, u64)], hits: &[u64], misses: &[u64], samples: usize) -> Metrics {
    let build = || {
        let mut map = BTreeMap::<u64, u64>::new();
        for &(key, value) in ops {
            let _ = map.insert(key, value);
        }
        map
    };

    let mut insert_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let mut map = BTreeMap::<u64, u64>::new();
        for &(key, value) in ops {
            black_box(map.insert(key, value));
        }
        insert_samples.push(start.elapsed());
        black_box(&map);
        drop(map);
    }

    let map = build();

    let mut hit_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let mut checksum = 0u64;
        for &key in hits {
            if let Some(value) = map.get(&key) {
                checksum = checksum.wrapping_add(*value);
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
            if let Some(value) = map.get(&key) {
                checksum = checksum.wrapping_add(*value);
            }
        }
        miss_samples.push(start.elapsed());
        black_box(checksum);
    }

    let mut iterate_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        let mut checksum = 0u64;
        for (key, value) in &map {
            checksum = checksum.wrapping_add(*key).wrapping_add(*value);
        }
        iterate_samples.push(start.elapsed());
        black_box(checksum);
    }

    let mut remove_samples = Vec::with_capacity(samples);
    for _ in 0..samples {
        let mut map = build();
        let start = Instant::now();
        for &(key, _) in ops {
            black_box(map.remove(&key));
        }
        remove_samples.push(start.elapsed());
        black_box(&map);
        drop(map);
    }

    let n = ops.len().max(1);
    Metrics {
        insert: per_op_ns(median(insert_samples), n),
        lookup_hit: per_op_ns(median(hit_samples), n),
        lookup_miss: per_op_ns(median(miss_samples), n),
        iterate: per_op_ns(median(iterate_samples), n),
        remove: per_op_ns(median(remove_samples), n),
    }
}

fn print_row(
    name: &str,
    m4: &Metrics,
    m16: &Metrics,
    btree: &Metrics,
    select: impl Fn(&Metrics) -> f64,
) {
    println!(
        "{:<14}{:>16.2}{:>17.2}{:>16.2}",
        name,
        select(m4),
        select(m16),
        select(btree)
    );
}

fn print_ratio(
    name: &str,
    m4: &Metrics,
    m16: &Metrics,
    btree: &Metrics,
    select: impl Fn(&Metrics) -> f64,
) {
    let base = select(btree);
    println!(
        "{:<14}{:>15.2}x{:>16.2}x",
        name,
        select(m4) / base,
        select(m16) / base
    );
}
