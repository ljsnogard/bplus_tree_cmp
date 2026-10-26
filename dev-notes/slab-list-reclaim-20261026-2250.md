# SlabList 空 Chunk 归还（20261026-2250）

## 背景

此前 `SlabList` 只会在 `clear` 时释放 Chunk。树的删除操作即使把某个 Chunk 的
全部槽位都归还，Chunk 仍会留在链表中；树缩小后这部分内存不会回到 allocator。

本轮先实现 SlabList 级别的局部归还机制，为后续的 Chunk 大小配置和碎片控制
打基础。

## 归还策略

`remove` 归还槽位后，如果该 Chunk 变为完全空闲：

1. 若它是链表中唯一的 Chunk，保留；
2. 否则检查前驱和后继：
   - 前驱或后继完全空闲时，把当前 Chunk 从链表摘下，调用 allocator 释放；
   - 两个邻居都不完全空闲时，保留当前 Chunk 并移到链表头部，等待复用。

该策略是一次局部决策，不承诺一次操作就把所有相邻空闲 Chunk 都回收。保留
“唯一 Chunk”可以避免 `remove` 后紧接 `insert` 时立即重新分配，形成抖动。

### 自动化触发

归还不是一个需要调用者显式调用的 `release` / `shrink` 操作。`remove` 在把某个
槽位放回空闲链表、使 `free_count_ == capacity_` 的那个调用中，会直接执行
`reclaim_empty_chunk_`；检查邻居与 `deallocate` 都发生在同一次 `remove` 内。
调用者只是执行普通的删除；allocator 参数是 `deallocate` 的必要输入，由树在
内部自动传入，不代表调用者可以选择“稍后手动归还”。


## 接口变化

释放 Chunk 必须使用分配它时的 allocator，因此内部接口：

```rust
SlabList::remove(ptr, &C::Allocator) -> T
```

`SlabList` 仍然不保存 allocator 实例，allocator 仍由树持有；`remove` 与
`clear` 一样在需要时借用 `&C::Allocator`。

树侧只发生内部参数传递变化：

- B+ Tree：删除路径把 `&self.allocator_` 传给 `remove_entry_with_comparer_`，
  再传给 `rebalance_after_remove_`、`clear_root_`、`collapse_root_to_child_`；
- B Tree：删除递归把 allocator 传给 `remove_rec_`、`take_min_`、
  `rebalance_child_`、`merge_with_left_` / `merge_with_right_`。

`SlabList` 是 crate 私有类型，因此树的公开删除 API 不受影响。

## 链表不变量

- 链表头部始终可分配：唯一 Chunk 被保留；有可用槽位的 Chunk 会被移到头部。
- 空链表只会发生在 `new` 或 `clear` 后，所有 Chunk 由 `clear` 统一释放。
- 归还 Chunk 时先摘链，再调用 allocator；不会在释放后访问该 Chunk。

## 测试

- 新增 4 个 SlabList 单元测试：
  - 最后一个非空闲槽位归还后，在同一次 `remove` 内自动释放；
  - 前驱空闲时归还当前 Chunk；
  - 后继空闲时归还当前 Chunk；
  - 唯一 Chunk 完全空闲时保留。
- 新增 2 个树级 allocator 集成测试：B+ Tree 与 B Tree 在删除全部元素的过程中
  释放部分 Chunk，同时保留至少一个 Chunk；drop 后释放总数等于分配总数。
- 既有 `clear` / drop / 随机增删测试全部通过。
- Miri（strict provenance）通过 SlabList 归还测试、B Tree 顺序删除和 B+ Tree
  半删/全删测试。

## 后续

- 将 Chunk 大小作为公开配置项时，仍沿用本轮的局部归还策略。
- 若需要“删除后尽可能归还内存”，可以再增加 `try_shrink` 或更激进的链式回收；
  本轮不改变局部策略的语义。
