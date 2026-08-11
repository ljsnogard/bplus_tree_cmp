# BPlusTreeCmp

A small Rust in memory B+tree map whose ordering is defined entirely by a comparer supplied at construction time.
All memory allocated by `slab`, an arena allocator for Rust.

The important design point is deliberately simple:

```rust
BPlusTree<K, V, C, N>
                ^
                comparer defines K < K
```

`K` does **not** need to implement `Ord`, `PartialOrd`, `Eq`, or any other ordering trait. The map only needs an `C: Trcomparer<K>` capable of comparing two keys.

## Example

```rust
use btree_cmp::{BPlusTree, OrdComparer};

let mut map = BPlusTree::new(OrdComparer);
map.insert(3, "three");
map.insert(1, "one");
map.insert(2, "two");

assert_eq!(map.get(&2), Some(&"two"));
```

Or pass a closure:

```rust
use btree_cmp::BPlusTree;

#[derive(Debug)]
struct Key(String);

let mut map = BPlusTree::new(|a: &Key, b: &Key| a.0.cmp(&b.0));
```

The comparer may also carry state.

## Current scope

This is intentionally a compact first implementation of the core B+tree algorithm:

- user-supplied comparer stored in the tree
- no ordering trait bound on `T`
- top-down insertion with node splitting
- lookup
- mutable lookup
- replacement of an existing key
- minimum/maximum lookup
- invariant checking

Deletion, iterators, ranges, entry APIs, allocator support, and heterogeneous query comparers are intentionally left for subsequent stages. The deletion method is currently a placeholder and should not be considered implemented.

## comparer contract

The comparer must define the ordering used by the tree. In particular, for keys stored in the tree it must be consistent and transitive. If the comparer is inconsistent, search and insertion results are logically invalid; this is a caller contract rather than a type-level restriction.
