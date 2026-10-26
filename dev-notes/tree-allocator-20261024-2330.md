# 两棵树公开 Allocator 泛型（20261024-2330）

## 背景

此前 `SlabList` 自己保存 allocator，而 `BPlusTree` / `BTree` 固定使用默认配置
`SlabListConfig<DEFAULT_CHUNK_BYTES>` 与 `Global`，allocator 无法替换。本轮为
两棵树增加公开的最后一位泛型参数：

```rust
pub struct BPlusTree<const M: usize, C, K, V = (), A = Global>;
pub struct BTree<const M: usize, C, K, V = (), A = Global>;
```

`A` 指定内置 `SlabList` 使用的底层 allocator，默认值 `Global` 保证现有写法仍然
成立。

## 技术决策

### 1. 公开 API 采用 std::collections 风格

`new_in` 中的 `_in` 只表示 allocator；带 comparer 的入口改名为
`with_comparer`。`K: Ord` 时不需要显式 comparer，自动使用
`OrdComparer<K>`。

两棵树对称提供：

```text
new()                                   K: Ord + Global
new_in(allocator)                       K: Ord + 自定义 A
with_comparer(comparer)                 Global + 自定义 C
with_comparer_in(comparer, allocator)   自定义 C + 自定义 A
from_iter_in(iter, allocator)           K: Ord + 自定义 A
from_iter_with_comparer(comparer, iter) Global + 自定义 C
from_iter_with_comparer_in(comparer, iter, allocator)
FromIterator                             K: Ord + OrdComparer + Global
```

所有 `_by` 方法同步改名为 `_with_comparer`：

```text
get_by                  -> get_with_comparer
get_mut_by              -> get_mut_with_comparer
try_insert_by           -> try_insert_with_comparer
remove_by               -> remove_with_comparer
remove_entry_by         -> remove_entry_with_comparer
from_iter_by            -> from_iter_with_comparer
```

`FromIterator` 不再依赖 `C: Default`，而是专门实现于
`BPlusTree<M, OrdComparer<K>, K, V, Global>` / `BTree<...>`，语义与 `K: Ord`
的便捷构造器一致。

### 2. SlabList 不长期保存 allocator

B+ Tree 有两个 arena，若要求 `A: Clone`，带状态的 allocator 会被不必要地限制。
但 Rust 不能在同一个结构体中安全地同时保存 `allocator_: A` 和指向该字段的
`&A`：结构体移动会使引用失效。

最终采用：

- 树持有唯一的 `allocator_: A`；
- `SlabList` 不再保存 allocator；
- `SlabList::insert` 接收 `&C::Allocator`，用于按需分配新 Chunk；
- `SlabList::clear` 接收 `&C::Allocator`，用于析构存活元素并释放所有 Chunk；
- `SlabList` 的 `Drop` 只做 `debug_assert!`，要求调用方先 `clear`；树在自己的
  `Drop` 中、allocator 仍然存活时清空两个 arena。

因此：

- `A` 不需要 `Clone`，也不需要 `Default`；
- 树仍然完全拥有 allocator；
- 没有 `Rc` / `Arc` / 自引用 `unsafe`；
- 树的 `Send` 条件只要求 `A: Send`，与直接持有 `A` 一致。

代价是 `SlabList` 的内部构造/清理接口从“构造时传入 allocator”改为“分配与清理时
借用 allocator”。`SlabList` 是 crate 内部类型，不影响公开 API。

### 3. Drop 顺序

两棵树的 `Drop` 先调用 `clear(&self.allocator_)`：

- B+ Tree 先清 data arena（析构 `(K, V)`），再清 node arena；
- B Tree 清 node arena（析构节点中的 `(K, V)`）。

随后字段按声明顺序释放，allocator 在 arena 之后或之前释放都不会被已清空的
`SlabList` 访问。

## 公开 API 变化摘要

- 新增最后一个泛型参数 `A = Global`；
- 构造器改为 `new` / `new_in(allocator)` / `with_comparer` /
  `with_comparer_in(comparer, allocator)`；
- 所有接受额外 comparer 的方法改名为 `*_with_comparer`；
- `from_iter_by` 改名为 `from_iter_with_comparer`，并新增 `from_iter_in` /
  `from_iter_with_comparer_in`；
- `FromIterator` 面向 `K: Ord + Global`；
- `SlabList` 保持 crate 内部，但改为按需借用 allocator。

## 验证

- `cargo test`：58 个单元测试、9 个集成测试、23 个文档测试全部通过；
- 新增 `tests/allocator.rs`：用**未实现 `Clone`** 的 `CountingAlloc` 验证两棵树
  的 allocator 分配/释放，以及 `with_comparer_in`；
- `cargo clippy --all-targets`：0 警告；
- `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps`：0 警告；
- `rustfmt` 仅作用于本轮修改的文件。

> 后续变更（20261026-2305）：树的最后一个泛型参数由 `A: Allocator` 改为
> `Cfg: TrConfig`，同时指定 Chunk 大小与 allocator。`TrConfig`、
> `SlabListConfig`、`DefaultSlabConfig`、`DEFAULT_CHUNK_BYTES` 已公开导出。
> 详见 `dev-notes/tree-config-20261026-2305.md`。
