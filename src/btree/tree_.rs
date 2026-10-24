//! 基于 Comparer 定序的 B Tree。
//!
//! 与 [`crate::BPlusTree`] 的公开 API 保持对称，但内部结构是标准 B 树：
//! 每个节点都可以保存实际数据，而不是只在叶子保存数据。
//!
//! # 结构
//!
//! - 数据直接保存在节点中，每个 `(K, V)` 只存在一份；
//! - 节点全部由 `SlabList<Node<M, K, V>, ...>` 分配，不做逐节点堆分配；
//! - allocator 类型由公开泛型参数 `A` 指定，默认 `Global`；
//! - 每个节点保存 `parent_` 父指针，迭代器据此计算中序后继；
//! - 内部节点最多 `M` 个项、`M + 1` 个孩子；
//! - 非根节点至少 `ceil((M + 1) / 2) - 1` 个项，根节点豁免；
//! - 所有叶子位于同一层。
//!
//! # Comparer 一致性
//!
//! 树中元素的顺序由构造时传入的 `C` 固定。`get_with_comparer` / `remove_entry_with_comparer` /
//! `try_insert_with_comparer` 等接受额外 Comparer 的方法，其 Comparer 必须在 `K ↔ K`
//! 上与 `C` 一致，否则查找和插入位置没有意义。
//!
//! # 示例
//!
//! ```
//! use bplustree_cmp::BTree;
//!
//! let mut tree = BTree::<4, _, u64, &str>::new().unwrap();
//! tree.insert(2, "two");
//! tree.insert(1, "one");
//! tree.insert(3, "three");
//!
//! let pairs: std::vec::Vec<_> = tree.iter().map(|(k, v)| (*k, *v)).collect();
//! assert_eq!(pairs, std::vec![(1, "one"), (2, "two"), (3, "three")]);
//! ```

use alloc::alloc::Global;
use core::{alloc::Allocator, borrow::Borrow, mem};

use crate::{
    comparer_::{OrdComparer, TrComparer},
    slab_list_::DefaultSlabList,
    tree_error_::TreeError,
};

use super::node_::{Node, TreeNodePtr, entry_mut_, entry_ref_};

use super::{
    insert_::insert_pair_into_tree_,
    iter_::TreeIter,
    remove_::remove_entry_from_tree_,
    search_::{find_entry_, first_entry_, last_entry_},
};

/// 删除核心逻辑。
///
/// `query_cmp` 用于与查询对象比较，`key_cmp` 用于节点内部排序。
fn remove_entry_impl_<const M: usize, K, V, Q, QC, C, A>(
    nodes: &mut DefaultSlabList<Node<M, K, V>, A>,
    root: &mut Option<TreeNodePtr<M, K, V>>,
    len: &mut usize,
    query: &Q,
    query_cmp: &QC,
    key_cmp: &C,
) -> Option<(K, V)>
where
    Q: ?Sized,
    QC: TrComparer<K, Q>,
    C: TrComparer<K>,
    A: Allocator,
{
    let removed = remove_entry_from_tree_(nodes, root, query, query_cmp, key_cmp);

    if removed.is_some() {
        *len -= 1;
    }
    removed
}

/// 基于 Comparer 定序的 B Tree。
///
/// 该结构与 [`crate::BPlusTree`] 提供同一组公开操作，区别在于数据分布在
/// 所有节点中，而不是只保存在叶子。`K` 不需要 `Ord`，也不要求 `Clone`；
/// 查询对象可以与 `K` 不同。
///
/// 泛型参数：
///
/// - `M`：每个节点的最大项数，必须至少为 2；
/// - `C`：定义 `K` 排序关系的 comparer 类型；
/// - `K` / `V`：键与值类型；
/// - `A`：内置 `SlabList` 使用的 allocator 类型，默认 [`Global`]。
///
/// 树自身持有 `A` 类型的 allocator 实例；`SlabList` 只在分配/释放 Chunk 时
/// 借用 `&A`，因此不要求 `A: Clone`。
///
/// # 地址稳定性
///
/// `SlabList` 保证的是节点槽位稳定，而不是节点内部 `(K, V)` 的地址稳定。
/// B 树的分裂、借位和合并会按值移动项：项可能从原节点移动到新节点、父节点或
/// 兄弟节点。因此，修改操作之后不承诺同一个 `&K` 仍指向原来的地址。与之相对，
/// [`crate::BPlusTree`] 把 `(K, V)` 放在独立的 Data SlabList 中，索引节点只
/// 移动 `SlotPtr`，所以数据地址在元素被删除前保持不变。
///
/// 构造入口：
///
/// - `K: Ord` 时可用 [`BTree::new`] / [`BTree::new_in`]；
/// - 自定义 comparer 可用 [`BTree::with_comparer`] / [`BTree::with_comparer_in`]。
///
/// # 示例
///
/// ```
/// use bplustree_cmp::BTree;
///
/// let mut tree = BTree::<4, _, i32, i32>::new().unwrap();
/// tree.insert(2, 20);
/// tree.insert(1, 10);
///
/// assert_eq!(tree.get(2), Some((&2, &20)));
/// assert_eq!(tree.first(), Some((&1, &10)));
/// ```
pub struct BTree<const M: usize, C, K, V = (), A = Global>
where
    C: TrComparer<K>,
    A: Allocator,
{
    len_: usize,
    comparer_: C,
    tree_root_: Option<TreeNodePtr<M, K, V>>,
    allocator_: A,
    node_arena_: DefaultSlabList<Node<M, K, V>, A>,
}

impl<const M: usize, C, K, V, A> Drop for BTree<M, C, K, V, A>
where
    C: TrComparer<K>,
    A: Allocator,
{
    fn drop(&mut self) {
        // SlabList 不持有 allocator，必须在 allocator 仍然存活时归还 Chunk。
        self.node_arena_.clear(&self.allocator_);
    }
}

impl<const M: usize, C, K, V, A> BTree<M, C, K, V, A>
where
    C: TrComparer<K>,
    A: Allocator,
{
    /// 返回节点最大项数 `M`。
    ///
    /// 内部节点最多有 `M + 1` 个孩子；非根节点至少有
    /// `ceil((M + 1) / 2) - 1` 个项。
    pub const fn degree(&self) -> usize {
        M
    }

    /// 使用给定 comparer 和 allocator 创建空树。
    ///
    /// `SlabList` 在分配/释放 Chunk 时按需借用 allocator，因此不要求
    /// `A: Clone`。
    fn new_with_allocator_(comparer: C, allocator: A) -> Result<Self, TreeError> {
        assert!(M >= 2, "B 树的节点容量 M 必须至少为 2");
        Ok(Self {
            len_: 0,
            comparer_: comparer,
            tree_root_: Option::None,
            allocator_: allocator,
            node_arena_: DefaultSlabList::new().map_err(TreeError::from_slab_)?,
        })
    }

    /// 返回元素数量。
    #[inline]
    pub fn len(&self) -> usize {
        self.len_
    }

    /// 返回树是否为空。
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len_ == 0
    }

    /// 返回 comparer 的引用。
    #[inline]
    pub fn comparer(&self) -> &C {
        &self.comparer_
    }

    /// 返回最小 `(K, V)` 引用。
    ///
    /// B 树的第一个元素可能位于内部节点，因此需要沿最左路径下降到叶子，
    /// 复杂度为 `O(log n)`。
    pub fn first(&self) -> Option<(&K, &V)> {
        let root = self.tree_root_?;
        let (node_ptr, idx) = first_entry_(root)?;
        // SAFETY: node_ptr 由树维护；&self 保证返回引用期间节点存活。
        Option::Some(unsafe { entry_ref_(node_ptr, idx) })
    }

    /// 返回最大 `(K, V)` 引用。
    ///
    /// 复杂度为 `O(log n)`，原因与 [`Self::first`] 相同。
    pub fn last(&self) -> Option<(&K, &V)> {
        let root = self.tree_root_?;
        let (node_ptr, idx) = last_entry_(root)?;
        // SAFETY: node_ptr 由树维护；&self 保证返回引用期间节点存活。
        Option::Some(unsafe { entry_ref_(node_ptr, idx) })
    }

    /// 使用存储的 comparer 查询与 `query` 等价的元素。
    ///
    /// `Q: Borrow<K>` 允许用 `&K` 或其他能借出 `K` 的类型查询。若需要
    /// 用完全不同的查询类型，请使用 [`Self::get_with_comparer`]。
    pub fn get<Q>(&self, query: Q) -> Option<(&K, &V)>
    where
        Q: Borrow<K>,
    {
        let query_ref = query.borrow();
        self.get_with_comparer(query_ref, &self.comparer_)
    }

    /// 使用存储的 comparer 查询并返回可变 value 引用。
    pub fn get_mut<Q>(&mut self, query: Q) -> Option<(&K, &mut V)>
    where
        Q: Borrow<K>,
    {
        let query_ref = query.borrow();
        let root = self.tree_root_?;
        let (node_ptr, idx) = find_entry_(root, query_ref, &self.comparer_)?;
        // SAFETY: node_ptr 由树维护；&mut self 保证独占访问。
        Option::Some(unsafe { entry_mut_(node_ptr, idx) })
    }

    /// 使用异构 comparer 查询与 `query` 等价的元素。
    ///
    /// `cmp` 必须在 `K ↔ K` 上与树的存储 Comparer 一致。
    pub fn get_with_comparer<'f, Q, TyCmp>(
        &'f self,
        query: &Q,
        cmp: &TyCmp,
    ) -> Option<(&'f K, &'f V)>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        let root = self.tree_root_?;
        let (node_ptr, idx) = find_entry_(root, query, cmp)?;
        // SAFETY: node_ptr 由树维护；&self 保证期间节点存活。
        Option::Some(unsafe { entry_ref_(node_ptr, idx) })
    }

    /// 使用异构 comparer 查询并返回可变 value 引用。
    pub fn get_mut_with_comparer<'f, Q, TyCmp>(
        &'f mut self,
        query: &Q,
        cmp: &TyCmp,
    ) -> Option<(&'f K, &'f mut V)>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        let root = self.tree_root_?;
        let (node_ptr, idx) = find_entry_(root, query, cmp)?;
        // SAFETY: node_ptr 由树维护；&mut self 保证独占访问。
        Option::Some(unsafe { entry_mut_(node_ptr, idx) })
    }

    /// 使用异构 comparer 插入一个新元素。
    ///
    /// `factory` 只在确认不存在等价 key 后调用一次；插入成功时返回新元素的
    /// `(&K, &mut V)`。若已存在等价 key，返回 [`ConflictInfo`]，且
    /// `conflict` 为 `None`（factory 未被调用）。
    ///
    /// `factory` 返回的 `K` 必须与 `query` 在 `cmp` 下比较等价，并且 `cmp` 在
    /// `K ↔ K` 上必须与树的存储 Comparer 一致，否则树无法保证有序性。
    ///
    /// # Errors
    ///
    /// 当树中已经存在与 `query` 等价的 key 时返回 [`ConflictInfo`]。
    pub fn try_insert_with_comparer<'f, Q, TyCmp>(
        &'f mut self,
        query: &Q,
        cmp: &TyCmp,
        factory: impl FnOnce(&Q) -> (K, V),
    ) -> Result<(&'f K, &'f mut V), ConflictInfo<'f, K, V>>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        if let Option::Some(root) = self.tree_root_
            && let Option::Some((node_ptr, idx)) = find_entry_(root, query, cmp)
        {
            // SAFETY: node_ptr 由树维护；&mut self 保证独占访问。
            let (key, value) = unsafe { entry_mut_(node_ptr, idx) };
            return Err(ConflictInfo {
                existing_: (key, value),
                conflict_: Option::None,
            });
        }

        let pair = factory(query);
        let location = insert_pair_into_tree_(
            &mut self.node_arena_,
            &mut self.tree_root_,
            pair,
            &self.comparer_,
            &self.allocator_,
        );
        let (node_ptr, idx) = match location {
            Ok(location) => location,
            Err(_) => unreachable!("插入前已经确认不存在等价 key"),
        };

        self.len_ += 1;
        // SAFETY: node_ptr 指向刚插入的项；&mut self 保证独占访问。
        let (key, value) = unsafe { entry_mut_(node_ptr, idx) };
        Ok((key, value))
    }

    /// 使用存储的 comparer 插入一个已构造好的 `(K, V)`。
    ///
    /// 插入成功返回 `(&K, &mut V)`；若已存在等价 key，返回
    /// [`ConflictInfo`]，其 `conflict` 为 `Some((K, V))`，包含调用方传入的
    /// 键值对。
    ///
    /// # Errors
    ///
    /// 当树中已经存在与 `key` 等价的 key 时返回 [`ConflictInfo`]。
    pub fn try_insert<'f>(
        &'f mut self,
        key: K,
        value: V,
    ) -> Result<(&'f K, &'f mut V), ConflictInfo<'f, K, V>> {
        let pair = (key, value);

        if let Option::Some(root) = self.tree_root_
            && let Option::Some((node_ptr, idx)) = find_entry_(root, &pair.0, &self.comparer_)
        {
            // SAFETY: node_ptr 由树维护；&mut self 保证独占访问。
            let (existing_key, existing_value) = unsafe { entry_mut_(node_ptr, idx) };
            return Err(ConflictInfo {
                existing_: (existing_key, existing_value),
                conflict_: Option::Some(pair),
            });
        }

        let location = insert_pair_into_tree_(
            &mut self.node_arena_,
            &mut self.tree_root_,
            pair,
            &self.comparer_,
            &self.allocator_,
        );
        let (node_ptr, idx) = match location {
            Ok(location) => location,
            Err(_) => unreachable!("插入前已经确认不存在等价 key"),
        };

        self.len_ += 1;
        // SAFETY: node_ptr 指向刚插入的项；&mut self 保证独占访问。
        let (key, value) = unsafe { entry_mut_(node_ptr, idx) };
        Ok((key, value))
    }

    /// 插入 `(K, V)`；若 key 已存在则覆盖 value。
    ///
    /// 返回被覆盖的旧 value；不覆盖时返回 `None`。旧 key 保持不变。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BTree;
    ///
    /// let mut tree = BTree::<4, _, i32, &str>::new().unwrap();
    /// assert_eq!(tree.insert(1, "one"), None);
    /// assert_eq!(tree.insert(1, "uno"), Some("one"));
    /// assert_eq!(tree.get(1), Some((&1, &"uno")));
    /// ```
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        match self.try_insert(key, value) {
            Ok(_) => Option::None,
            Err(info) => {
                let ((_, existing_value), conflict) = info.into_parts();
                match conflict {
                    Option::Some((_, value)) => Option::Some(mem::replace(existing_value, value)),
                    Option::None => Option::None,
                }
            }
        }
    }

    /// 删除与 `query` 等价的元素，返回被删除的 value。
    pub fn remove<Q>(&mut self, query: Q) -> Option<V>
    where
        Q: Borrow<K>,
    {
        self.remove_entry(query).map(|(_, value)| value)
    }

    /// 删除与 `query` 等价的元素，返回完整的 `(K, V)`。
    pub fn remove_entry<Q>(&mut self, query: Q) -> Option<(K, V)>
    where
        Q: Borrow<K>,
    {
        let query_ref = query.borrow();
        remove_entry_impl_(
            &mut self.node_arena_,
            &mut self.tree_root_,
            &mut self.len_,
            query_ref,
            &self.comparer_,
            &self.comparer_,
        )
    }

    /// 使用异构 comparer 删除与 `query` 等价的元素，返回 value。
    pub fn remove_with_comparer<Q, TyCmp>(&mut self, query: &Q, cmp: &TyCmp) -> Option<V>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        self.remove_entry_with_comparer(query, cmp)
            .map(|(_, value)| value)
    }

    /// 使用异构 comparer 删除与 `query` 等价的元素，返回完整 `(K, V)`。
    ///
    /// `cmp` 必须在 `K ↔ K` 上与树的存储 Comparer 一致。
    pub fn remove_entry_with_comparer<Q, TyCmp>(&mut self, query: &Q, cmp: &TyCmp) -> Option<(K, V)>
    where
        Q: ?Sized,
        TyCmp: TrComparer<K, Q>,
    {
        remove_entry_impl_(
            &mut self.node_arena_,
            &mut self.tree_root_,
            &mut self.len_,
            query,
            cmp,
            &self.comparer_,
        )
    }

    /// 返回按 key 升序迭代的迭代器。
    ///
    /// B 树数据分散在所有节点中，因此迭代器需要一条按树高分配的临时工作栈。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BTree;
    ///
    /// let mut tree = BTree::<4, _, i32, i32>::new().unwrap();
    /// tree.insert(2, 20);
    /// tree.insert(1, 10);
    /// let keys: Vec<i32> = tree.iter().map(|(key, _)| *key).collect();
    /// assert_eq!(keys, vec![1, 2]);
    /// ```
    pub fn iter(&self) -> TreeIter<'_, M, K, V> {
        TreeIter::new_(self.tree_root_, self.len_)
    }
}

impl<const M: usize, K, V> BTree<M, OrdComparer<K>, K, V, Global>
where
    K: Ord,
{
    /// 使用 `OrdComparer` 与 [`Global`] allocator 创建一个空树。
    ///
    /// 这是 [`Self::new_in`] 在默认 allocator 下的便捷形式。
    ///
    /// # Errors
    ///
    /// 若默认 SlabList 配置无法容纳至少一个节点，返回 [`TreeError`]。
    ///
    /// # Panics
    ///
    /// 当 `M < 2` 时 panic：B 树至少需要能容纳两个项的节点。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BTree;
    ///
    /// let tree = BTree::<4, _, i32, i32>::new().unwrap();
    /// assert!(tree.is_empty());
    /// ```
    pub fn new() -> Result<Self, TreeError> {
        Self::new_in(Global)
    }
}

impl<const M: usize, K, V, A> BTree<M, OrdComparer<K>, K, V, A>
where
    K: Ord,
    A: Allocator,
{
    /// 使用 `OrdComparer` 与给定 allocator 创建一个空树。
    ///
    /// allocator 由树自身持有；`SlabList` 在分配或释放 Chunk 时借用该
    /// allocator，因此不要求 `A: Clone`。
    ///
    /// # Errors
    ///
    /// 若默认 SlabList 配置无法容纳至少一个节点，返回 [`TreeError`]。
    ///
    /// # Panics
    ///
    /// 当 `M < 2` 时 panic：B 树至少需要能容纳两个项的节点。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BTree;
    /// use std::alloc::Global;
    ///
    /// let tree = BTree::<4, _, i32, i32>::new_in(Global).unwrap();
    /// assert!(tree.is_empty());
    /// ```
    pub fn new_in(allocator: A) -> Result<Self, TreeError> {
        Self::new_with_allocator_(OrdComparer::new(), allocator)
    }

    /// 使用 `OrdComparer` 与给定 allocator 从已有 `(K, V)` 集合构建树。
    ///
    /// 重复 key 保留最后一次出现的 value。
    ///
    /// # Errors
    ///
    /// 与 [`Self::new_in`] 相同。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::BTree;
    /// use std::alloc::Global;
    ///
    /// let tree = BTree::<4, _, i32, i32>::from_iter_in(vec![(2, 20), (1, 10)], Global).unwrap();
    /// assert_eq!(tree.len(), 2);
    /// ```
    pub fn from_iter_in(
        iter: impl IntoIterator<Item = (K, V)>,
        allocator: A,
    ) -> Result<Self, TreeError> {
        let mut tree = Self::new_in(allocator)?;
        tree.extend(iter);
        Ok(tree)
    }
}

impl<const M: usize, C, K, V> BTree<M, C, K, V, Global>
where
    C: TrComparer<K>,
{
    /// 使用给定 comparer 与 [`Global`] allocator 创建一个空树。
    ///
    /// `K` 不需要实现 `Ord`；排序完全由 `comparer` 决定。
    ///
    /// # Errors
    ///
    /// 若默认 SlabList 配置无法容纳至少一个节点，返回 [`TreeError`]。
    ///
    /// # Panics
    ///
    /// 当 `M < 2` 时 panic：B 树至少需要能容纳两个项的节点。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{BTree, OrdComparer};
    ///
    /// let tree = BTree::<4, _, i32, i32>::with_comparer(OrdComparer::new()).unwrap();
    /// assert!(tree.is_empty());
    /// ```
    pub fn with_comparer(comparer: C) -> Result<Self, TreeError> {
        Self::with_comparer_in(comparer, Global)
    }

    /// 使用给定 comparer 与 [`Global`] allocator 从已有集合构建树。
    ///
    /// 重复 key 保留最后一次出现的 value。
    ///
    /// # Errors
    ///
    /// 与 [`Self::with_comparer`] 相同。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{BTree, OrdComparer};
    ///
    /// let tree = BTree::<4, _, i32, i32>::from_iter_with_comparer(
    ///     OrdComparer::new(),
    ///     vec![(2, 20), (1, 10)],
    /// )
    /// .unwrap();
    /// assert_eq!(tree.len(), 2);
    /// assert_eq!(tree.first(), Some((&1, &10)));
    /// ```
    pub fn from_iter_with_comparer(
        comparer: C,
        iter: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Self, TreeError> {
        let mut tree = Self::with_comparer(comparer)?;
        tree.extend(iter);
        Ok(tree)
    }
}

impl<const M: usize, C, K, V, A> BTree<M, C, K, V, A>
where
    C: TrComparer<K>,
    A: Allocator,
{
    /// 使用给定 comparer 与 allocator 创建一个空树。
    ///
    /// allocator 由树自身持有；`SlabList` 在分配或释放 Chunk 时借用该
    /// allocator，因此不要求 `A: Clone`。
    ///
    /// # Errors
    ///
    /// 若默认 SlabList 配置无法容纳至少一个节点，返回 [`TreeError`]。
    ///
    /// # Panics
    ///
    /// 当 `M < 2` 时 panic：B 树至少需要能容纳两个项的节点。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{BTree, OrdComparer};
    /// use std::alloc::Global;
    ///
    /// let tree = BTree::<4, _, i32, i32>::with_comparer_in(OrdComparer::new(), Global).unwrap();
    /// assert!(tree.is_empty());
    /// ```
    pub fn with_comparer_in(comparer: C, allocator: A) -> Result<Self, TreeError> {
        Self::new_with_allocator_(comparer, allocator)
    }

    /// 使用给定 comparer 与 allocator 从已有集合构建树。
    ///
    /// 重复 key 保留最后一次出现的 value。
    ///
    /// # Errors
    ///
    /// 与 [`Self::with_comparer_in`] 相同。
    ///
    /// # Examples
    ///
    /// ```
    /// use bplustree_cmp::{BTree, OrdComparer};
    /// use std::alloc::Global;
    ///
    /// let tree = BTree::<4, _, i32, i32>::from_iter_with_comparer_in(
    ///     OrdComparer::new(),
    ///     vec![(2, 20), (1, 10)],
    ///     Global,
    /// )
    /// .unwrap();
    /// assert_eq!(tree.len(), 2);
    /// ```
    pub fn from_iter_with_comparer_in(
        comparer: C,
        iter: impl IntoIterator<Item = (K, V)>,
        allocator: A,
    ) -> Result<Self, TreeError> {
        let mut tree = Self::with_comparer_in(comparer, allocator)?;
        tree.extend(iter);
        Ok(tree)
    }
}

/// 插入冲突信息。
///
/// - [`BTree::try_insert_with_comparer`] 在冲突时不调用 factory，因此 `conflict` 为
///   `None`；
/// - [`BTree::try_insert`] 已经持有调用方传入的键值对，因此 `conflict` 为
///   `Some((K, V))`。
#[derive(Debug)]
pub struct ConflictInfo<'a, K, V = ()> {
    /// 树中已存在的键值引用。
    existing_: (&'a K, &'a mut V),
    /// 未能插入的键值对（如果调用方已经构造了它）。
    conflict_: Option<(K, V)>,
}

impl<'a, K, V> ConflictInfo<'a, K, V> {
    /// 返回树中已存在的 key 与 value 引用。
    pub fn existing(&self) -> (&K, &V) {
        (self.existing_.0, &*self.existing_.1)
    }

    /// 返回未能插入的键值对引用。
    pub fn conflict(&self) -> Option<&(K, V)> {
        self.conflict_.as_ref()
    }

    /// 拆分为已存在引用与未插入的键值对。
    pub fn into_parts(self) -> ((&'a K, &'a mut V), Option<(K, V)>) {
        (self.existing_, self.conflict_)
    }

    /// 返回树中已存在的 key 与可变 value 引用。
    pub fn into_existing(self) -> (&'a K, &'a mut V) {
        self.existing_
    }

    /// 返回未能插入的键值对。
    pub fn into_conflict(self) -> Option<(K, V)> {
        self.conflict_
    }
}

impl<const M: usize, C, K, V, A> Extend<(K, V)> for BTree<M, C, K, V, A>
where
    C: TrComparer<K>,
    A: Allocator,
{
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (key, value) in iter {
            let _ = self.insert(key, value);
        }
    }
}

/// 使用 [`OrdComparer`] 与 [`Global`] allocator 从 `(K, V)` 迭代器构建树。
///
/// 重复 key 保留最后出现的 value。若默认配置无法容纳节点，本实现会 panic；
/// 需要可失败版本时使用 [`BTree::from_iter_in`]。
impl<const M: usize, K, V> FromIterator<(K, V)> for BTree<M, OrdComparer<K>, K, V, Global>
where
    K: Ord,
{
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        match Self::from_iter_in(iter, Global) {
            Ok(tree) => tree,
            Err(err) => panic!("使用 OrdComparer 构建 BTree 失败: {err}"),
        }
    }
}

/// 对 `&BTree` 的迭代器。
impl<'a, const M: usize, C, K, V, A> IntoIterator for &'a BTree<M, C, K, V, A>
where
    C: TrComparer<K>,
    A: Allocator,
{
    type Item = (&'a K, &'a V);
    type IntoIter = TreeIter<'a, M, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
impl<const M: usize, C, K, V, A> BTree<M, C, K, V, A>
where
    C: TrComparer<K>,
    A: Allocator,
{
    /// 检查 B 树结构与排序不变量。
    fn check_invariants_(&self) {
        let Option::Some(root) = self.tree_root_ else {
            assert_eq!(self.len_, 0);
            return;
        };

        let mut leaf_depth = Option::None;
        let count = self.check_node_(root, true, Option::None, Option::None, 1, &mut leaf_depth);
        assert_eq!(count, self.len_);
        assert!(leaf_depth.is_some(), "非空树应至少有一个叶子");
    }

    /// 递归检查一个节点及其子树。
    fn check_node_<'a>(
        &'a self,
        ptr: TreeNodePtr<M, K, V>,
        is_root: bool,
        lower: Option<&'a K>,
        upper: Option<&'a K>,
        depth: usize,
        leaf_depth: &mut Option<usize>,
    ) -> usize {
        // SAFETY: ptr 由树维护；&'a self 保证检查期间节点存活。
        let node: &'a Node<M, K, V> = unsafe { super::node_::node_ref_(ptr) };
        let len = node.len_();
        let is_leaf = node.is_leaf_();

        if is_root {
            assert!(len > 0, "非空树的 root 不应为空");
            assert!(node.parent_().is_none(), "根节点的父指针应为 None");
        } else {
            assert!(
                len >= super::node_::min_keys_(M),
                "非根节点低于最小占用: len = {len}, min = {}",
                super::node_::min_keys_(M)
            );
        }

        if is_leaf {
            assert!(node.first_child_.is_none(), "叶子节点不应持有孩子");
            match *leaf_depth {
                Option::None => *leaf_depth = Option::Some(depth),
                Option::Some(expected) => assert_eq!(depth, expected, "所有叶子应位于同一层"),
            }
        } else {
            assert!(node.first_child_.is_some(), "内部节点应持有第一个孩子");
        }

        let mut count = 0usize;
        for i in 0..len {
            let (key, node_value) = node.entries_.get_ref_at(i).expect("节点下标应有效");

            if let Option::Some(lower) = lower {
                assert_eq!(
                    self.comparer_.compare(lower, key),
                    core::cmp::Ordering::Less,
                    "key 应大于下界"
                );
            }
            if let Option::Some(upper) = upper {
                assert_eq!(
                    self.comparer_.compare(key, upper),
                    core::cmp::Ordering::Less,
                    "key 应小于上界"
                );
            }
            if i > 0 {
                let (prev, _) = node.entries_.get_ref_at(i - 1).expect("节点下标应有效");
                assert_eq!(
                    self.comparer_.compare(prev, key),
                    core::cmp::Ordering::Less,
                    "节点内 key 应严格递增"
                );
            }

            if is_leaf {
                assert!(node_value.right_.is_none(), "叶子项不应持有右孩子");
            } else {
                assert!(node_value.right_.is_some(), "内部项应持有右孩子");
            }
            count += 1;
        }

        if !is_leaf {
            for i in 0..=len {
                let child = node.child_at_(i).expect("内部节点孩子下标应有效");
                assert_eq!(
                    unsafe { super::node_::node_ref_(child) }.parent_(),
                    Option::Some(ptr),
                    "孩子的父指针应指向当前节点"
                );
                let child_lower = if i == 0 {
                    lower
                } else {
                    match node.entries_.get_ref_at(i - 1) {
                        Ok((key, _)) => Option::Some(key),
                        Err(_) => unreachable!("节点下标应有效"),
                    }
                };
                let child_upper = if i == len {
                    upper
                } else {
                    match node.entries_.get_ref_at(i) {
                        Ok((key, _)) => Option::Some(key),
                        Err(_) => unreachable!("节点下标应有效"),
                    }
                };
                count += self.check_node_(
                    child,
                    false,
                    child_lower,
                    child_upper,
                    depth + 1,
                    leaf_depth,
                );
            }
        }

        count
    }
}

#[cfg(test)]
mod tests_tree_ {
    use core::cmp::Ordering;

    use std::{
        collections::BTreeMap,
        string::{String, ToString},
        vec,
        vec::Vec,
    };

    use super::*;
    use crate::comparer_::OrdComparer;

    /// 测试目标：验证空树的查询、首尾与迭代行为。
    /// Test goal: Verify query, first/last, and iteration behavior of an empty tree.
    ///
    /// 测试手段：创建空树后调用 get / first / last / remove / iter。
    /// Test method: Create an empty tree and call get / first / last / remove / iter.
    ///
    /// 判定标准：查询与删除返回 None，首尾为 None，迭代器立即结束，len 为 0。
    /// Assertions: Query and removal return None, first/last are None, the iterator
    /// ends immediately, and len is zero.
    #[test]
    fn test_empty_tree() {
        let mut tree = BTree::<4, _, i32, i32>::new().unwrap();

        assert_eq!(tree.len(), 0);
        assert!(tree.is_empty());
        assert_eq!(tree.degree(), 4);
        assert_eq!(tree.first(), None);
        assert_eq!(tree.last(), None);
        assert_eq!(tree.get(1), None);
        assert_eq!(tree.remove(1), None);
        assert_eq!(tree.iter().count(), 0);
        tree.check_invariants_();
    }

    /// 测试目标：验证升序/降序插入后的查询、首尾与有序迭代。
    /// Test goal: Verify query, first/last, and ordered iteration after ascending and
    /// descending insertion.
    ///
    /// 测试手段：分别插入 0..64 和 64..0，检查每个 key 的查询结果以及迭代顺序。
    /// Test method: Insert 0..64 and 64..0 separately, checking every key lookup and
    /// the iteration order.
    ///
    /// 判定标准：所有查询命中正确 value，first/last 正确，迭代严格升序，结构不变量成立。
    /// Assertions: Every lookup returns the expected value, first/last are correct,
    /// iteration is strictly ascending, and structural invariants hold.
    #[test]
    fn test_insert_get_and_order() {
        for reversed in [false, true] {
            let mut tree = BTree::<4, _, i32, i32>::new().unwrap();
            let keys: Vec<i32> = if reversed {
                (0..64).rev().collect()
            } else {
                (0..64).collect()
            };

            for key in keys {
                assert_eq!(tree.insert(key, key * 10), None);
            }

            assert_eq!(tree.len(), 64);
            assert_eq!(tree.first(), Some((&0, &0)));
            assert_eq!(tree.last(), Some((&63, &630)));

            for key in 0..64 {
                assert_eq!(tree.get(key), Some((&key, &(key * 10))));
            }

            let collected: Vec<i32> = tree.iter().map(|(key, _)| *key).collect();
            assert_eq!(collected, (0..64).collect::<Vec<_>>());
            tree.check_invariants_();
        }
    }

    /// 测试目标：验证重复 key 的冲突信息与覆盖语义。
    /// Test goal: Verify conflict information and overwrite semantics for duplicate keys.
    ///
    /// 测试手段：先 try_insert 插入一次，再用相同 key 调用 try_insert 与 insert。
    /// Test method: Insert once with try_insert, then call try_insert and insert again
    /// with the same key.
    ///
    /// 判定标准：第二次 try_insert 返回 ConflictInfo，conflict 包含传入的 pair；
    /// insert 返回旧 value，长度不变，key 仍为原 key。
    /// Assertions: The second try_insert returns ConflictInfo whose conflict holds the
    /// supplied pair; insert returns the old value, the length is unchanged, and the
    /// stored key stays the original one.
    #[test]
    fn test_duplicate_insert() {
        let mut tree = BTree::<4, _, i32, &'static str>::new().unwrap();

        assert!(tree.try_insert(1, "one").is_ok());

        let conflict = tree.try_insert(1, "uno").unwrap_err();
        assert_eq!(conflict.existing(), (&1, &"one"));
        assert_eq!(conflict.conflict(), Some(&(1, "uno")));
        assert_eq!(tree.len(), 1);

        assert_eq!(tree.insert(1, "eins"), Some("one"));
        assert_eq!(tree.get(1), Some((&1, &"eins")));
        assert_eq!(tree.len(), 1);
        tree.check_invariants_();
    }

    /// 测试目标：验证待插入项上移到父节点时，`try_insert` / `try_insert_with_comparer` 返回的
    /// 引用仍指向正确项。
    /// Test goal: Verify that the references returned by `try_insert` / `try_insert_with_comparer`
    /// still point at the inserted entry when it moves up into a parent node.
    ///
    /// 测试手段：在 `M = 2` 的树中先插入 1 和 3，再插入 2；插入 2 会使根分裂并把
    /// 2 上移到新的根节点。随后通过返回的 `&mut V` 修改 value。
    /// Test method: With `M = 2`, insert 1 and 3 first, then insert 2. The insertion
    /// splits the root and moves 2 into the new root. The returned `&mut V` is then
    /// used to mutate the value.
    ///
    /// 判定标准：两次插入 API 都返回 key 2 的正确引用；通过引用修改 value 后，
    /// `get(2)` 能看到新 value，结构不变量成立。
    /// Assertions: Both insertion APIs return the correct reference for key 2;
    /// mutating through it is visible through `get(2)`, and invariants hold.
    #[test]
    fn test_try_insert_returned_position_when_moved_up() {
        let mut tree = BTree::<2, _, i32, i32>::new().unwrap();
        assert!(tree.try_insert(1, 10).is_ok());
        assert!(tree.try_insert(3, 30).is_ok());

        let (key, value) = tree.try_insert(2, 20).expect("应插入成功");
        assert_eq!(*key, 2);
        assert_eq!(*value, 20);
        *value = 200;
        assert_eq!(tree.get(2), Some((&2, &200)));
        tree.check_invariants_();

        let mut tree = BTree::<2, _, i32, i32>::new().unwrap();
        let cmp = OrdComparer::new();
        assert!(
            tree.try_insert_with_comparer(&1, &cmp, |key| (*key, 10))
                .is_ok()
        );
        assert!(
            tree.try_insert_with_comparer(&3, &cmp, |key| (*key, 30))
                .is_ok()
        );

        let (key, value) = tree
            .try_insert_with_comparer(&2, &cmp, |key| (*key, 20))
            .expect("应插入成功");
        assert_eq!(*key, 2);
        assert_eq!(*value, 20);
        *value = 200;
        assert_eq!(tree.get(2), Some((&2, &200)));
        tree.check_invariants_();
    }

    /// 测试目标：验证按顺序删除全部元素时，借位、合并与根收缩始终正确。
    /// Test goal: Verify that borrowing, merging, and root collapsing stay correct
    /// while removing every element in order.
    ///
    /// 测试手段：分别插入 0..128 后按升序删除，插入 0..128 后按降序删除；
    /// 每删除一个 key 都检查首尾与结构不变量。
    /// Test method: Insert 0..128 and remove in ascending order, then insert 0..128 and
    /// remove in descending order, checking first/last and invariants after every
    /// removal.
    ///
    /// 判定标准：每次删除返回正确 value，first/last 随剩余元素更新，最终树为空且
    /// 不变量始终成立。
    /// Assertions: Each removal returns the expected value, first/last track the
    /// remaining entries, the tree finally becomes empty, and invariants always hold.
    #[test]
    fn test_remove_sequential() {
        for reversed in [false, true] {
            let mut tree = BTree::<3, _, i32, i32>::new().unwrap();
            for key in 0..128 {
                let _ = tree.insert(key, key * 3);
            }

            let order: Vec<i32> = if reversed {
                (0..128).rev().collect()
            } else {
                (0..128).collect()
            };

            for (step, key) in order.iter().copied().enumerate() {
                assert_eq!(tree.remove(key), Some(key * 3));
                assert_eq!(tree.len(), 128 - step - 1);
                if !tree.is_empty() {
                    let expected_first = if reversed { 0 } else { key + 1 };
                    let expected_last = if reversed { key - 1 } else { 127 };
                    assert_eq!(tree.first().map(|(key, _)| *key), Some(expected_first));
                    assert_eq!(tree.last().map(|(key, _)| *key), Some(expected_last));
                }
                tree.check_invariants_();
            }

            assert!(tree.is_empty());
            assert_eq!(tree.first(), None);
            assert_eq!(tree.last(), None);
            tree.check_invariants_();
        }
    }

    /// 测试目标：用随机操作与 `BTreeMap` 模型对拍，检查增删查与结构不变量。
    /// Test goal: Differential-test random operations against a `BTreeMap` model,
    /// covering insertion, removal, lookup, and structural invariants.
    ///
    /// 测试手段：使用固定种子的 xorshift 在多组节点容量 `M` 上生成插入/删除/查询，
    /// key 空间为 0..256；每次操作后比较长度、首尾与查询结果，并检查完整不变量。
    /// Test method: Use a fixed-seed xorshift over several node capacities `M` to
    /// generate inserts, removals, and lookups in the key space 0..256, comparing
    /// lengths, first/last, and lookups after each operation, and checking full
    /// invariants.
    ///
    /// 判定标准：树与模型的长度、首尾、查询、删除返回值完全一致；周期性完整迭代
    /// 顺序一致；结构不变量始终成立。
    /// Assertions: Lengths, first/last, lookups, and removal results match the model;
    /// periodic full iteration order matches; structural invariants always hold.
    #[test]
    fn test_random_against_btreemap() {
        let degrees: &[usize] = if cfg!(miri) {
            &[2, 4]
        } else {
            &[2, 3, 4, 5, 6]
        };
        for &degree in degrees {
            random_model_test_(degree);
        }
    }

    fn random_model_test_(degree: usize) {
        match degree {
            2 => random_model_for_::<2>(),
            3 => random_model_for_::<3>(),
            4 => random_model_for_::<4>(),
            5 => random_model_for_::<5>(),
            6 => random_model_for_::<6>(),
            _ => unreachable!(),
        }
    }

    fn random_model_for_<const M: usize>() {
        let mut tree = BTree::<M, _, i32, i32>::new().unwrap();
        let mut model = BTreeMap::<i32, i32>::new();
        let mut state = 0x1234_5678_9abc_def0u64;

        let steps = if cfg!(miri) { 40 } else { 1200 };
        for step in 0..steps {
            let key = (next_rand_(&mut state) % 256) as i32;
            match next_rand_(&mut state) % 3 {
                0 => {
                    let value = key * 7 + step;
                    let old = tree.insert(key, value);
                    let expected = model.insert(key, value);
                    assert_eq!(old, expected);
                }
                1 => {
                    let actual = tree.remove(key);
                    let expected = model.remove(&key);
                    assert_eq!(actual, expected);
                }
                _ => {
                    let actual = tree.get(key).map(|(key, value)| (*key, *value));
                    let expected = model.get(&key).map(|value| (key, *value));
                    assert_eq!(actual, expected);
                }
            }

            assert_eq!(tree.len(), model.len());
            assert_eq!(
                tree.first().map(|(key, value)| (*key, *value)),
                model.first_key_value().map(|(key, value)| (*key, *value))
            );
            assert_eq!(
                tree.last().map(|(key, value)| (*key, *value)),
                model.last_key_value().map(|(key, value)| (*key, *value))
            );
            tree.check_invariants_();

            if step % 97 == 0 {
                let actual: Vec<(i32, i32)> =
                    tree.iter().map(|(key, value)| (*key, *value)).collect();
                let expected: Vec<(i32, i32)> =
                    model.iter().map(|(key, value)| (*key, *value)).collect();
                assert_eq!(actual, expected);
            }
        }
    }

    fn next_rand_(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[derive(Debug)]
    struct User {
        id: u64,
        name: String,
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

    /// 测试目标：验证异构查询、factory 构造与按 `u64` 删除。
    /// Test goal: Verify heterogeneous queries, factory construction, and removal by
    /// a `u64` query.
    ///
    /// 测试手段：树中保存 `User`，按 `User.id` 排序；用 `u64` hint 与
    /// `TrComparer<User, u64>` 插入、查询和删除。
    /// Test method: Store `User` ordered by `User.id`; insert, query, and remove using
    /// a `u64` hint and `TrComparer<User, u64>`.
    ///
    /// 判定标准：插入后能按 id 查到对应 User；按 id 删除返回正确 User；树中不需要
    /// 构造临时 User 即可完成查询。
    /// Assertions: Inserted users are found by id; removal by id returns the correct
    /// user; queries work without constructing a temporary `User`.
    #[test]
    fn test_heterogeneous_query() {
        let mut tree =
            BTree::<4, _, User, u64>::with_comparer(UserIdCmp).expect("默认配置应当有效");
        let query_cmp = UserIdQueryCmp;

        for id in 0..64u64 {
            let result = tree.try_insert_with_comparer(&id, &query_cmp, |id| {
                (
                    User {
                        id: *id,
                        name: "user".to_string(),
                    },
                    *id * 10,
                )
            });
            assert!(result.is_ok());
        }

        assert_eq!(tree.len(), 64);
        for id in 0..64u64 {
            let (user, value) = tree
                .get_with_comparer(&id, &query_cmp)
                .expect("按 id 应能查到 User");
            assert_eq!(user.id, id);
            assert_eq!(user.name, "user");
            assert_eq!(*value, id * 10);
        }

        let removed = tree
            .remove_entry_with_comparer(&32u64, &query_cmp)
            .expect("按 id 应能删除 User");
        assert_eq!(removed.0.id, 32);
        assert_eq!(removed.1, 320);
        assert!(tree.get_with_comparer(&32u64, &query_cmp).is_none());
        tree.check_invariants_();
    }

    struct RevCmp;

    impl TrComparer<i32> for RevCmp {
        fn compare(&self, a: &i32, b: &i32) -> Ordering {
            b.cmp(a)
        }
    }

    /// 测试目标：验证排序关系完全由 Comparer 决定。
    /// Test goal: Verify that ordering is defined entirely by the comparer.
    ///
    /// 测试手段：使用反向 comparer 插入 0..32，并执行查询与删除。
    /// Test method: Insert 0..32 with a reverse comparer, then query and remove.
    ///
    /// 判定标准：first 为最大值、last 为最小值，迭代严格降序，查找与删除仍然正确。
    /// Assertions: first is the maximum, last is the minimum, iteration is strictly
    /// descending, and lookups and removals still work.
    #[test]
    fn test_reverse_comparer() {
        let mut tree = BTree::<4, _, i32, i32>::with_comparer(RevCmp).unwrap();
        for key in 0..32 {
            let _ = tree.insert(key, key);
        }

        assert_eq!(tree.first().map(|(key, _)| *key), Some(31));
        assert_eq!(tree.last().map(|(key, _)| *key), Some(0));
        let keys: Vec<i32> = tree.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, (0..32).rev().collect::<Vec<_>>());
        assert_eq!(tree.get(7), Some((&7, &7)));
        assert_eq!(tree.remove(7), Some(7));
        assert_eq!(tree.get(7), None);
        tree.check_invariants_();
    }

    #[derive(Debug)]
    struct NoCloneKey {
        id: u64,
    }

    struct NoCloneCmp;

    impl TrComparer<NoCloneKey> for NoCloneCmp {
        fn compare(&self, a: &NoCloneKey, b: &NoCloneKey) -> Ordering {
            a.id.cmp(&b.id)
        }
    }

    /// 测试目标：验证 Key 不需要 `Clone`，节点移动到新位置时也不复制 Key。
    /// Test goal: Verify that keys do not need `Clone` and that moving entries between
    /// nodes never copies keys.
    ///
    /// 测试手段：使用没有实现 `Clone` 的 `NoCloneKey` 插入、查询并删除数百个元素；
    /// 插入量足以触发多层分裂与合并。
    /// Test method: Use `NoCloneKey` (which does not implement `Clone`) to insert,
    /// query, and remove hundreds of entries, enough to trigger several levels of
    /// splits and merges.
    ///
    /// 判定标准：代码可编译（证明无 Clone 要求），查询与删除结果正确，不变量成立。
    /// Assertions: The code compiles (proving no `Clone` requirement), lookups and
    /// removals return the expected results, and invariants hold.
    #[test]
    fn test_no_clone_key() {
        let mut tree =
            BTree::<4, _, NoCloneKey, u64>::with_comparer(NoCloneCmp).expect("默认配置应当有效");

        for id in 0..256u64 {
            let result = tree.try_insert(NoCloneKey { id }, id * 3);
            assert!(result.is_ok());
        }

        for id in 0..256u64 {
            let (key, value) = tree
                .get_with_comparer(&id, &NoCloneIdQueryCmp)
                .expect("key 应存在");
            assert_eq!(key.id, id);
            assert_eq!(*value, id * 3);
        }

        for id in (0..256u64).step_by(2) {
            let removed = tree
                .remove_entry_with_comparer(&id, &NoCloneIdQueryCmp)
                .expect("key 应能删除");
            assert_eq!(removed.0.id, id);
            assert_eq!(removed.1, id * 3);
        }
        tree.check_invariants_();
    }

    struct NoCloneIdQueryCmp;

    impl TrComparer<NoCloneKey, u64> for NoCloneIdQueryCmp {
        fn compare(&self, a: &NoCloneKey, b: &u64) -> Ordering {
            a.id.cmp(b)
        }
    }

    /// 测试目标：验证 `from_iter_with_comparer`、`FromIterator` 与 `Extend` 的构建语义。
    /// Test goal: Verify the construction semantics of `from_iter_with_comparer`,
    /// `FromIterator`, and `Extend`.
    ///
    /// 测试手段：用含重复 key 的集合分别通过 `from_iter_with_comparer` 与 `collect` 构建树，
    /// 再 `extend` 新元素。
    /// Test method: Build trees from a collection containing duplicate keys through
    /// both `from_iter_with_comparer` and `collect`, then `extend` them with new elements.
    ///
    /// 判定标准：重复 key 保留最后 value；extend 后长度和查询正确；迭代有序。
    /// Assertions: Duplicate keys keep the last value; length and lookups are correct
    /// after extend; iteration is ordered.
    #[test]
    fn test_from_iter_and_extend() {
        let items = vec![(3, "three"), (1, "one"), (2, "two"), (1, "uno")];

        let mut tree = BTree::<4, _, i32, &'static str>::from_iter_with_comparer(
            OrdComparer::new(),
            items.clone(),
        )
        .expect("默认配置应当有效");
        assert_eq!(tree.len(), 3);
        assert_eq!(tree.get(1), Some((&1, &"uno")));

        let mut collected: BTree<4, OrdComparer<i32>, i32, &'static str> =
            items.into_iter().collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected.get(1), Some((&1, &"uno")));

        tree.extend(vec![(4, "four"), (0, "zero")]);
        assert_eq!(tree.len(), 5);
        let keys: Vec<i32> = tree.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, vec![0, 1, 2, 3, 4]);
        tree.check_invariants_();

        collected.extend(vec![(5, "five")]);
        assert_eq!(collected.len(), 4);
        collected.check_invariants_();
    }

    /// 测试目标：验证可变查询、冲突更新与 insert 的覆盖语义。
    /// Test goal: Verify mutable lookup, conflict updates, and insert overwrite
    /// semantics.
    ///
    /// 测试手段：插入三个元素，通过 `get_mut` 修改 value，通过 `ConflictInfo`
    /// 修改已有 value，再用 `insert` 覆盖并检查返回值。
    /// Test method: Insert three elements, mutate a value through `get_mut`, mutate
    /// an existing value through `ConflictInfo`, then overwrite with `insert` and
    /// check the returned old value.
    ///
    /// 判定标准：三种修改都生效，长度不变，旧 value 返回正确。
    /// Assertions: All three mutations take effect, the length stays unchanged, and
    /// the old value is returned correctly.
    #[test]
    fn test_get_mut_and_conflict_update() {
        let mut tree = BTree::<4, _, i32, i32>::new().unwrap();
        for key in 0..3 {
            let _ = tree.insert(key, key * 10);
        }

        if let Some((key, value)) = tree.get_mut(1) {
            assert_eq!(*key, 1);
            *value = 11;
        } else {
            panic!("get_mut 应命中");
        }
        assert_eq!(tree.get(1), Some((&1, &11)));

        let conflict = tree.try_insert(1, 99).unwrap_err();
        assert_eq!(conflict.conflict(), Some(&(1, 99)));
        {
            let ((key, value), pair) = conflict.into_parts();
            assert_eq!(*key, 1);
            assert_eq!(pair, Some((1, 99)));
            *value = 12;
        }
        assert_eq!(tree.get(1), Some((&1, &12)));

        assert_eq!(tree.insert(1, 13), Some(12));
        assert_eq!(tree.get(1), Some((&1, &13)));
        assert_eq!(tree.len(), 3);
        tree.check_invariants_();
    }

    /// 测试目标：验证 `try_insert_with_comparer` 只在确实插入时调用 factory。
    /// Test goal: Verify that `try_insert_with_comparer` calls the factory only when an
    /// insertion actually happens.
    ///
    /// 测试手段：用 `Cell` 统计 factory 调用次数，先插入重复 key，再插入新 key。
    /// Test method: Count factory calls with a `Cell`, first inserting a duplicate
    /// key and then a new key.
    ///
    /// 判定标准：重复插入时调用次数不增加，新 key 插入时恰好增加一次；冲突信息
    /// 的 `conflict` 为 `None`。
    /// Assertions: The call count does not increase for a duplicate, increases by
    /// exactly one for a new key, and conflict information has `conflict == None`.
    #[test]
    fn test_factory_only_on_success() {
        let mut tree = BTree::<4, _, i32, i32>::new().unwrap();
        let cmp = OrdComparer::new();
        let calls = std::cell::Cell::new(0usize);

        let result = tree.try_insert_with_comparer(&1, &cmp, |_| {
            calls.set(calls.get() + 1);
            (1, 10)
        });
        assert!(result.is_ok());
        assert_eq!(calls.get(), 1);

        let conflict = tree
            .try_insert_with_comparer(&1, &cmp, |_| {
                calls.set(calls.get() + 1);
                (1, 99)
            })
            .unwrap_err();
        assert_eq!(calls.get(), 1);
        assert!(conflict.conflict().is_none());
        assert_eq!(conflict.existing(), (&1, &10));

        let result = tree.try_insert_with_comparer(&2, &cmp, |_| {
            calls.set(calls.get() + 1);
            (2, 20)
        });
        assert!(result.is_ok());
        assert_eq!(calls.get(), 2);
        tree.check_invariants_();
    }

    struct StringStrCmp;

    impl TrComparer<String, str> for StringStrCmp {
        fn compare(&self, a: &String, b: &str) -> Ordering {
            a.as_str().cmp(b)
        }
    }

    /// 测试目标：验证 `str` 这类 unsized 查询对象可以直接用于异构查询。
    /// Test goal: Verify that unsized query types such as `str` can be used directly
    /// for heterogeneous lookup.
    ///
    /// 测试手段：树中保存 `String`，使用 `TrComparer<String, str>` 进行插入、
    /// 查询、可变查询与删除。
    /// Test method: Store `String` and use `TrComparer<String, str>` for insertion,
    /// lookup, mutable lookup, and removal.
    ///
    /// 判定标准：`&str` 查询全部命中正确元素，删除返回正确 value。
    /// Assertions: All `&str` queries hit the correct element and removal returns
    /// the expected value.
    #[test]
    fn test_unsized_str_query() {
        let mut tree = BTree::<4, _, String, i32>::new().unwrap();
        let cmp = StringStrCmp;

        for text in ["alpha", "beta", "gamma"] {
            let result = tree
                .try_insert_with_comparer(text, &cmp, |text| (text.to_string(), text.len() as i32));
            assert!(result.is_ok());
        }

        assert_eq!(
            tree.get_with_comparer("beta", &cmp)
                .map(|(key, value)| (key.as_str(), *value)),
            Some(("beta", 4))
        );

        if let Some((key, value)) = tree.get_mut_with_comparer("beta", &cmp) {
            assert_eq!(key.as_str(), "beta");
            *value = 40;
        } else {
            panic!("get_mut_with_comparer 应命中");
        }
        assert_eq!(
            tree.get_with_comparer("beta", &cmp)
                .map(|(_, value)| *value),
            Some(40)
        );

        assert_eq!(tree.remove_with_comparer("alpha", &cmp), Some(5));
        assert!(tree.get_with_comparer("alpha", &cmp).is_none());
        tree.check_invariants_();
    }

    /// 测试目标：验证 `remove` 返回 value、`remove_entry` 返回完整键值对。
    /// Test goal: Verify that `remove` returns the value and `remove_entry` returns
    /// the full key-value pair.
    ///
    /// 测试手段：插入两个元素，分别调用 `remove` 与 `remove_entry`。
    /// Test method: Insert two elements and call `remove` and `remove_entry`
    /// respectively.
    ///
    /// 判定标准：`remove` 返回旧 value，`remove_entry` 返回 `(K, V)`，之后两个 key
    /// 都不可查询。
    /// Assertions: `remove` returns the old value, `remove_entry` returns `(K, V)`,
    /// and both keys become unreachable afterwards.
    #[test]
    fn test_remove_value_and_entry() {
        let mut tree = BTree::<4, _, i32, i32>::new().unwrap();
        let _ = tree.insert(1, 10);
        let _ = tree.insert(2, 20);

        assert_eq!(tree.remove(1), Some(10));
        assert_eq!(tree.remove_entry(2), Some((2, 20)));
        assert_eq!(tree.len(), 0);
        assert!(tree.is_empty());
        tree.check_invariants_();
    }

    /// 测试目标：验证迭代器的精确长度、`size_hint` 与 fused 行为。
    /// Test goal: Verify the exact length, `size_hint`, and fused behavior of the
    /// iterator.
    ///
    /// 测试手段：插入 64 个元素，逐项调用 `next`，在每一步检查剩余长度与
    /// `size_hint`，结束后再次调用 `next`。
    /// Test method: Insert 64 entries, call `next` item by item, checking the
    /// remaining length and `size_hint` at each step, then call `next` again after
    /// exhaustion.
    ///
    /// 判定标准：初始 `len` 为 64，逐项递减到 0；迭代结束后 `next` 仍返回 `None`。
    /// Assertions: The initial `len` is 64 and decreases to zero one by one; `next`
    /// still returns `None` after exhaustion.
    #[test]
    fn test_iter_exact_size_and_fused() {
        let mut tree = BTree::<5, _, i32, i32>::new().unwrap();
        for key in 0..64 {
            let _ = tree.insert(key, key);
        }

        let mut iter = tree.iter();
        assert_eq!(iter.len(), 64);
        for expected in 0..64 {
            assert_eq!(iter.size_hint(), (64 - expected, Some(64 - expected)));
            assert_eq!(iter.next().map(|(key, _)| *key), Some(expected as i32));
        }
        assert_eq!(iter.len(), 0);
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
        tree.check_invariants_();
    }

    #[derive(Debug)]
    struct DropKey {
        id: u64,
        counter: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Drop for DropKey {
        fn drop(&mut self) {
            self.counter.set(self.counter.get() + 1);
        }
    }

    struct DropValue {
        counter: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Drop for DropValue {
        fn drop(&mut self) {
            self.counter.set(self.counter.get() + 1);
        }
    }

    struct DropKeyCmp;

    impl TrComparer<DropKey> for DropKeyCmp {
        fn compare(&self, a: &DropKey, b: &DropKey) -> Ordering {
            a.id.cmp(&b.id)
        }
    }

    struct DropQueryCmp;

    impl TrComparer<DropKey, u64> for DropQueryCmp {
        fn compare(&self, a: &DropKey, b: &u64) -> Ordering {
            a.id.cmp(b)
        }
    }

    /// 测试目标：验证分裂、借位、合并与删除过程中没有 Key/Value 的重复析构或泄漏。
    /// Test goal: Verify that splits, borrows, merges, and removals never double-drop
    /// or leak keys and values.
    ///
    /// 测试手段：让 key 与 value 都带析构计数器，插入 128 个元素，按 u64 查询
    /// 删除其中 43 个并立即析构返回的 pair，最后 drop 整棵树。
    /// Test method: Give both key and value drop counters, insert 128 entries, remove
    /// 43 of them by a `u64` query and drop the returned pairs immediately, then drop
    /// the whole tree.
    ///
    /// 判定标准：删除 43 个后计数为 86；整棵树 drop 后计数恰好为 256，说明每个
    /// 对象只析构一次，且空闲槽位没有被重复析构。
    /// Assertions: The counter is 86 after removing 43 entries and exactly 256 after
    /// dropping the tree, proving every object is dropped once and freed slots are
    /// not dropped again.
    #[test]
    fn test_drop_counts() {
        let counter = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let query_cmp = DropQueryCmp;

        {
            let mut tree = BTree::<4, _, DropKey, DropValue>::with_comparer(DropKeyCmp).unwrap();

            for id in 0..128u64 {
                let result = tree.try_insert(
                    DropKey {
                        id,
                        counter: std::rc::Rc::clone(&counter),
                    },
                    DropValue {
                        counter: std::rc::Rc::clone(&counter),
                    },
                );
                assert!(result.is_ok());
            }

            for id in (0..128u64).step_by(3) {
                let pair = tree
                    .remove_entry_with_comparer(&id, &query_cmp)
                    .expect("按 id 应能删除");
                drop(pair);
            }
            assert_eq!(counter.get(), 86);
            tree.check_invariants_();
        }

        assert_eq!(counter.get(), 256);
    }
}
