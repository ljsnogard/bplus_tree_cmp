use core::{cmp::Ordering, mem::MaybeUninit, ptr};

use crate::comparer_::{OrdComparer, TrComparer};

//-----------------------------------------------------------------------------
// About Insert conflict.
// Because OrderedArray is not designed to be an independant
// container but an helper type to simplify code in B+ tree. So it leaves the
// conflict by the upper layer user to decide what to do with insert conflict.
//-----------------------------------------------------------------------------

#[derive(Debug)]
pub struct ConflictInfo<'a, K, V = ()> {
    /// The index of the existing item
    at: usize,
    /// The mut ref to the existing item in the array
    existing: (&'a K, &'a mut V),
    /// The conflicting value that failed to insert.
    conflict: ConflictItem<'a, K, V>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictItem<'a, K, V = ()> {
    Key(&'a K),
    Pair((K, V)),
}

pub enum TryInsertResult<'a, K, V = ()> {
    Succ((&'a K, &'a mut V)),

    /// Failed due to array is full.
    Full(ConflictItem<'a, K, V>),

    /// Failed due to conflict with an existing item in the array.
    Conflict(ConflictInfo<'a, K, V>),
}

impl<'a, K, V> TryInsertResult<'a, K, V> {
    pub const fn try_succ(&self) -> Option<(&K, &V)> {
        let TryInsertResult::Succ(x) = self else {
            return Option::None;
        };
        let mapped = (x.0, x.1 as &V);
        Option::Some(mapped)
    }

    #[inline]
    pub const fn is_succ(&self) -> bool {
        self.try_succ().is_some()
    }

    pub const fn try_full(&self) -> Option<&ConflictItem<'_, K, V>> {
        let TryInsertResult::Full(x) = self else {
            return Option::None;
        };
        Option::Some(x)
    }

    #[inline]
    pub const fn is_full(&self) -> bool {
        self.try_full().is_some()
    }

    pub const fn try_conflict(&self) -> Option<&ConflictInfo<'_, K, V>> {
        let TryInsertResult::Conflict(info) = self else {
            return Option::None;
        };
        Option::Some(info)
    }

    pub const fn is_conflict(&self) -> bool {
        self.try_conflict().is_some()
    }
}

//-----------------------------------------------------------------------------
// OrderedArray
//-----------------------------------------------------------------------------

#[derive(Debug)]
pub struct OrderedArray<const N: usize, K, V = ()> {
    elems_: [MaybeUninit<(K, V)>; N],
    order_: [usize; N],
    count_: usize,
}

impl<const N: usize, K, V> OrderedArray<N, K, V> {
    /// 创建一个空的有序数组。
    pub const fn new() -> Self {
        Self {
            elems_: unsafe { MaybeUninit::uninit().assume_init() },
            order_: [0usize; N],
            count_: 0usize,
        }
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    /// 返回是否为空。
    pub const fn is_empty(&self) -> bool {
        self.count_ == 0
    }

    /// 返回当前元素个数。
    pub const fn len(&self) -> usize {
        self.count_
    }

    pub const fn get_ref_at(&self, i: usize) -> Result<(&K, &V), usize> {
        if i < self.count_ {
            let p = self.order_[i];
            let (k, v) = unsafe { self.elems_[p].assume_init_ref() };
            Result::Ok((k, v))
        } else {
            Result::Err(self.count_)
        }
    }

    pub const fn get_mut_at(&mut self, i: usize) -> Result<(&K, &mut V), usize> {
        if i < self.count_ {
            let p = self.order_[i];
            let (k, v) = unsafe { self.elems_[p].assume_init_mut() };
            Result::Ok((k as &_, v))
        } else {
            Result::Err(self.count_)
        }
    }

    pub fn first(&self) -> Option<(&K, &V)> {
        self.get_ref_at(0usize).ok()
    }

    pub fn last(&self) -> Option<(&K, &V)> {
        if self.count_ > 0 {
            self.get_ref_at(self.count_ - 1).ok()
        } else {
            Option::None
        }
    }

    /// 返回逻辑顺序的迭代器，产生元素引用。
    pub fn iter(&self) -> Iter<'_, N, K, V> {
        Iter {
            array: self,
            index: 0,
        }
    }

    /// 使用 T 自身的 Ord 作为 `try_insert_by` 的 comparer 然后调用之。
    pub fn try_insert<'f>(
        &'f mut self,
        hint: &'f K,
        factory: impl FnOnce(&K) -> (K, V),
    ) -> TryInsertResult<'f, K, V>
    where
        K: Ord,
    {
        let c = OrdComparer::new();
        self.try_insert_by(hint, &c, factory)
    }

    /// Insert a pair of (K, V), by the comparer on K.
    pub fn try_insert_pair_by<'f, TyCmp>(
        &'f mut self,
        pair: (K, V),
        cmp: &TyCmp,
    ) -> TryInsertResult<'f, K, V>
    where
        TyCmp: TrComparer<K>,
    {
        if self.count_ >= N {
            return TryInsertResult::Full(ConflictItem::Pair(pair));
        }
        // 0. 获得物理写入位置的索引
        let new_phys_idx = self.order_[self.count_];
        if self.count_ == 0 {
            for i in 0..N {
                self.order_[i] = i
            }
            self.elems_[0].write(pair);
            self.count_ += 1;
        } else {
            // 1. 在现有逻辑顺序（order[0..len]）中查找插入位置
            let curr_count = self.count_;
            let (hint, _) = &pair;
            let pos = self.lower_bound_by(hint, cmp);
            if pos < curr_count {
                let lb_phyx_idx = self.order_[pos];
                let (k, v) = unsafe { self.elems_[lb_phyx_idx].assume_init_mut() };

                if cmp.compare(k, hint) == Ordering::Equal {
                    return TryInsertResult::Conflict(ConflictInfo {
                        at: pos,
                        existing: (k as &_, v),
                        conflict: ConflictItem::Pair(pair),
                    });
                }
            }
            // 2. 移动 order 元素以腾出位置（从后往前移动）
            //    新索引 = len（旧长度），插入后总长度变为 len+1
            self.move_insert_position_(pos, new_phys_idx);

            // 3. 将新元素写入物理存储的末尾（索引 = 当前长度）
            self.elems_[new_phys_idx].write(pair);
            // 4. 更新长度
            self.count_ += 1;
        }
        let (k, v) = unsafe { self.elems_[new_phys_idx].assume_init_mut() };
        TryInsertResult::Succ((k as &_, v))
    }

    /// 尝试将元素插入有序数组。
    ///
    /// 新元素在物理存储上追加到当前可用槽位，
    /// 在逻辑顺序上插入到由 `cmp` 确定的位置。
    ///
    /// 如果数组已满，返回 `TryInsertResult::Full`。
    ///
    /// 如果已有元素与新元素在 `cmp` 意义下等价，
    /// 返回 `TryInsertResult::Conflict`，同时提供：
    /// - 已有元素的逻辑位置；
    /// - 已有元素的可变引用；
    /// - 未插入的新元素。
    ///
    /// `Conflict` 的具体处理方式由调用者决定，例如拒绝插入、
    /// 修改已有元素或使用新元素替换已有元素的部分字段。
    pub fn try_insert_by<'f, TyCmp>(
        &'f mut self,
        hint: &'f K,
        cmp: &TyCmp,
        factory: impl FnOnce(&K) -> (K, V),
    ) -> TryInsertResult<'f, K, V>
    where
        TyCmp: TrComparer<K>,
    {
        if self.count_ >= N {
            return TryInsertResult::Full(ConflictItem::Key(hint));
        }
        // 0. 获得物理写入位置的索引
        let new_phys_idx = self.order_[self.count_];
        if self.count_ == 0 {
            for i in 0..N {
                self.order_[i] = i
            }
            let kv = factory(hint);
            self.elems_[0].write(kv);
            self.count_ += 1;
        } else {
            // 1. 在现有逻辑顺序（order[0..len]）中查找插入位置
            let curr_count = self.count_;
            let pos = self.lower_bound_by(hint, cmp);

            if pos < curr_count {
                let lb_phyx_idx = self.order_[pos];
                let (k, v) = unsafe { self.elems_[lb_phyx_idx].assume_init_mut() };

                if cmp.compare(k, hint) == Ordering::Equal {
                    return TryInsertResult::Conflict(ConflictInfo {
                        at: pos,
                        existing: (k as &_, v),
                        conflict: ConflictItem::Key(hint),
                    });
                }
            }
            // 2. 移动 order 元素以腾出位置（从后往前移动）
            //    新索引 = len（旧长度），插入后总长度变为 len+1
            self.move_insert_position_(pos, new_phys_idx);

            // 3. 将新元素写入物理存储的末尾（索引 = 当前长度）
            self.elems_[new_phys_idx].write(factory(hint));
            // 4. 更新长度
            self.count_ += 1;
        }
        let (k, v) = unsafe { self.elems_[new_phys_idx].assume_init_mut() };
        TryInsertResult::Succ((k as &_, v))
    }

    /// 移除最后一个元素，也是唯一有效的移除方式。
    /// 这个方法不会影响下一次插入，
    /// 因为移除后会在最后一个索引位置记录被移除元素的物理索引。
    pub fn try_remove_last(&mut self) -> Option<(K, V)> {
        if self.count_ == 0 {
            return Option::None;
        }
        let last_phyx_ids = self.order_[self.count_ - 1];
        let x = &mut self.elems_[last_phyx_ids];
        self.count_ -= 1;
        Option::Some(unsafe { x.assume_init_read() })
    }

    /// 查找 comparer 意义下等价的元素。
    ///
    /// 找到时返回元素引用，否则返回 `None`。
    pub fn find_by<'f, TyHint, TyCmp>(
        &'f self,
        hint: &'f TyHint,
        cmp: &TyCmp,
    ) -> Option<(&'f K, &'f V)>
    where
        TyCmp: TrComparer<K, TyHint>,
    {
        if let Result::Ok(p) = self.binary_search_by(hint, cmp) {
            self.get_ref_at(p).ok()
        } else {
            Option::None
        }
    }

    /// 使用 Comparer 对有序数组执行二分查找。
    ///
    /// 找到 comparer 意义下等价的元素时返回 `Ok(index)`；
    /// 否则返回 `Err(index)`，其中 `index` 是该元素应该插入的位置。
    pub fn binary_search_by<TyHint, TyCmp>(
        &self,
        hint: &TyHint,
        cmp: &TyCmp,
    ) -> Result<usize, usize>
    where
        TyCmp: TrComparer<K, TyHint>,
    {
        let pos = self.lower_bound_by(hint, cmp);
        if pos == self.count_ {
            return Err(pos);
        }
        let phyx_idx = self.order_[pos];
        let (k, _) = unsafe { self.elems_[phyx_idx].assume_init_ref() };
        if cmp.compare(k, hint) == Ordering::Equal {
            Ok(pos)
        } else {
            Err(pos)
        }
    }

    /// 寻找第一个满足 elem >= query 的位置
    pub fn lower_bound_by<Q, C>(&self, query: &Q, cmp: &C) -> usize
    where
        C: TrComparer<K, Q>,
    {
        self.partition_point_(query, |elem, hint| {
            cmp.compare(elem, hint) == Ordering::Less
        })
    }

    /// 寻找第一个满足 elem > query 的位置
    pub fn upper_bound_by<Q, C>(&self, query: &Q, cmp: &C) -> usize
    where
        C: TrComparer<K, Q>,
    {
        self.partition_point_(query, |elem, hint| {
            matches!(cmp.compare(elem, hint), Ordering::Less | Ordering::Equal)
        })
    }

    /// 返回 [0, count_) 中第一个使 pred 返回 false 的位置。
    fn partition_point_<Q, P>(&self, query: &Q, mut pred: P) -> usize
    where
        P: FnMut(&K, &Q) -> bool,
    {
        let mut left = 0;
        let mut right = self.count_;

        while left < right {
            let mid = left + (right - left) / 2;
            let mid_phyx = self.order_[mid];
            let (k, _) = unsafe { self.elems_[mid_phyx].assume_init_ref() };

            if pred(k, query) {
                left = mid + 1;
            } else {
                right = mid;
            }
        }
        left
    }

    /// 在 orders 下表为 i 处插入 p，其他值往后移。
    /// 使用前必须先保证 self.count_ < N
    fn move_insert_position_(&mut self, i: usize, p: usize) {
        for i in (i..self.count_).rev() {
            self.order_[i + 1] = self.order_[i];
        }
        self.order_[i] = p;
    }
}

impl<const N: usize, T> OrderedArray<N, T, ()> {
    /// A convenient method that auto expands the item into a pair of (item, ())
    /// and then call `try_insert_pair_by`
    pub fn try_insert_item<'f, TyCmp>(
        &'f mut self,
        item: T,
        cmp: &TyCmp,
    ) -> TryInsertResult<'f, T, ()>
    where
        TyCmp: TrComparer<T>,
    {
        let pair = (item, ());
        self.try_insert_pair_by(pair, cmp)
    }
}

impl<const N: usize, T> OrderedArray<N, T, ()>
where
    T: Ord,
{
    /// A convenient method that ASSUMING the array using the OrdComparer or
    /// one of its compatible comparers, and then call `try_insert_pair_by`.
    pub fn try_insert_ord<'f>(&'f mut self, ord: T) -> TryInsertResult<'f, T, ()> {
        let cmp = OrdComparer::new();
        let pair = (ord, ());
        self.try_insert_pair_by(pair, &cmp)
    }
}

impl<const N: usize, K, V> OrderedArray<N, K, V>
where
    K: Ord,
{
    pub const fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, K, V> Default for OrderedArray<N, K, V> {
    fn default() -> Self {
        OrderedArray::new()
    }
}

/// 实现 Drop，正确释放已初始化的元素。
impl<const N: usize, K, V> Drop for OrderedArray<N, K, V> {
    fn drop(&mut self) {
        for i in 0..self.count_ {
            let phys_idx = self.order_[i];
            unsafe {
                ptr::drop_in_place(self.elems_[phys_idx].as_mut_ptr());
            }
        }
    }
}

//-----------------------------------------------------------------------------
// Iterator for OrderedArray
//-----------------------------------------------------------------------------

/// 迭代器：按逻辑顺序遍历元素。
pub struct Iter<'a, const N: usize, K, V> {
    array: &'a OrderedArray<N, K, V>,
    index: usize,
}

impl<'a, const N: usize, K, V> Iterator for Iter<'a, N, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.array.len() {
            let x = self.array.get_ref_at(self.index).ok();
            self.index += 1usize;
            x
        } else {
            Option::None
        }
    }
}

//-----------------------------------------------------------------------------
// Codes related to unit testings
//-----------------------------------------------------------------------------

#[cfg(test)]
pub(crate) fn from_test_items_by_<const M: usize, C, K, V>(
    items: impl core::iter::IntoIterator<Item = (K, V)>,
    cmp: &C,
) -> OrderedArray<M, K, V>
where
    K: Clone,
    C: TrComparer<K>,
{
    let mut it = items.into_iter();
    let mut arr = OrderedArray::<M, K, V>::new();
    while let Option::Some(t) = it.next() {
        let (k, v) = t;
        let x = arr.try_insert_by(&k, cmp, |_| (k.clone(), v));
        assert!(x.is_full());
    }
    arr
}

#[cfg(test)]
mod tests {
    use std::{
        string::{String, ToString},
        vec,
        vec::Vec,
    };

    use super::*;

    #[test]
    fn test_insert_and_order() {
        // let c = OrdComparer::default();
        let mut arr = OrderedArray::<5, i32>::default();
        assert!(arr.try_insert_ord(3).is_succ());
        assert!(arr.try_insert_ord(1).is_succ());
        assert!(arr.try_insert_ord(4).is_succ());
        assert!(arr.try_insert_ord(2).is_succ());

        let conflict = arr.try_insert_ord(1);
        assert!(conflict.is_conflict());
        let TryInsertResult::Conflict(x) = conflict else {
            unreachable!()
        };
        assert_eq!(x.at, 0usize);
        let (k, _) = x.existing;
        assert_eq!(*k, 1);
        assert_eq!(x.conflict, ConflictItem::Pair((1, ())));

        let expected = [1, 2, 3, 4];
        for (u, (i, _)) in arr.iter().enumerate() {
            assert_eq!(*i, expected[u])
        }
    }

    #[test]
    fn test_full() {
        // let c = OrdComparer::default();
        let mut arr = OrderedArray::<2, i32>::default();
        assert!(arr.try_insert_ord(10).is_succ());
        assert!(arr.try_insert_ord(20).is_succ());
        assert!(arr.try_insert_ord(30).is_full());
    }

    #[test]
    fn test_get() {
        fn map_as_str<'f>(t: (&'f String, &())) -> &'f str {
            t.0.as_str()
        }

        let mut arr = OrderedArray::<4, String>::default();
        assert!(arr.try_insert_ord("b".to_string()).is_succ());
        assert!(arr.try_insert_ord("a".to_string()).is_succ());
        assert!(arr.try_insert_ord("c".to_string()).is_succ());
        assert_eq!(arr.get_ref_at(0).map(map_as_str), Ok("a"));
        assert_eq!(arr.get_ref_at(1).map(map_as_str), Ok("b"));
        assert_eq!(arr.get_ref_at(2).map(map_as_str), Ok("c"));
        assert!(arr.get_ref_at(3).is_err());
    }

    fn select_key<'f, K, V>(t: (&'f K, &'f V)) -> &'f K {
        t.0
    }

    #[test]
    fn test_remove_last() {
        let mut arr = OrderedArray::<10, i32>::default();
        // let cmp = OrdComparer::new();

        arr.try_insert_ord(5);
        arr.try_insert_ord(2);
        arr.try_insert_ord(8);
        arr.try_insert_ord(1); // 当前顺序 [1, 2, 5, 8]

        let removed = arr.try_remove_last();
        assert_eq!(removed, Some((8, ())));
        assert_eq!(arr.len(), 3);

        let collected: Vec<i32> = arr.iter().map(select_key).copied().collect();
        assert_eq!(collected, vec![1, 2, 5]);

        // 再次删除
        let removed2 = arr.try_remove_last();
        assert_eq!(removed2, Some((5, ())));
        assert_eq!(arr.len(), 2);
        let collected2: Vec<i32> = arr.iter().map(select_key).copied().collect();
        assert_eq!(collected2, vec![1, 2]);
    }

    #[test]
    fn test_remove_then_insert() {
        let mut arr = OrderedArray::<10, i32>::default();
        // let cmp = OrdComparer::new();

        // 插入 [3, 1, 4, 5] → 逻辑顺序 [1, 3, 4, 5]
        arr.try_insert_ord(3);
        arr.try_insert_ord(1);
        arr.try_insert_ord(4);
        arr.try_insert_ord(5);
        assert_eq!(arr.len(), 4);

        // 删除最大值 5
        assert_eq!(arr.try_remove_last(), Some((5, ())));
        assert_eq!(arr.len(), 3);
        // 当前逻辑顺序 [1, 3, 4]

        // 插入 2 → 期望 [1, 2, 3, 4]
        arr.try_insert_ord(2);
        let collected: Vec<i32> = arr.iter().map(select_key).copied().collect();
        assert_eq!(collected, vec![1, 2, 3, 4]);

        // 删除最大值 4
        assert_eq!(arr.try_remove_last(), Some((4, ())));
        // 插入 6 → 期望 [1, 2, 3, 6]
        arr.try_insert_ord(6);
        let collected: Vec<i32> = arr.iter().map(select_key).copied().collect();
        assert_eq!(collected, vec![1, 2, 3, 6]);

        // 确保之前删除的 5 和 4 的位置被正确覆盖，没有残留数据
        // 通过有序性和长度验证即可
    }
}

#[cfg(test)]
mod tests_drop_ {
    use std::vec::Vec;

    use crate::TrComparer;

    use super::*;

    #[test]
    fn test_drop_cleans_up_arcs() {
        use std::sync::Arc;

        // 为 Arc<usize> 实现比较器（基于内部值比较）
        #[derive(Clone, Copy, Debug, Default)]
        struct ArcCmp;

        impl TrComparer<Arc<usize>> for ArcCmp {
            fn compare(&self, a: &Arc<usize>, b: &Arc<usize>) -> Ordering {
                a.cmp(b) // Arc 已实现 Ord（委托给内部值）
            }
        }

        let cmp = ArcCmp;
        let weaks = {
            let mut arr = OrderedArray::<10, Arc<usize>>::new();
            // let cmp = ArcCmp;
            let mut weak_vec = Vec::with_capacity(10);

            for i in 0..10 {
                let arc = Arc::new(i);
                weak_vec.push(Arc::downgrade(&arc)); // 插入前获取 Weak
                let res = arr.try_insert_pair_by((arc, ()), &cmp);
                assert!(matches!(res, TryInsertResult::Succ(_)));
            }
            // arr 在此作用域结束时被 drop
            weak_vec
        };

        // 所有弱引用应该已失效（因为 Arc 已被释放）
        for (i, w) in weaks.iter().enumerate() {
            assert!(w.upgrade().is_none(), "Weak at index {} should be dead", i);
        }
    }
}

#[cfg(test)]
mod tests_search_ {
    use super::*;

    #[test]
    fn test_partition_point() {
        let mut arr = OrderedArray::<8, i32>::new();

        for x in [10, 20, 30, 40, 50] {
            assert!(arr.try_insert_ord(x).is_succ());
        }

        // [10, 20, 30, 40, 50]
        let pred_lt = |x: &i32, q: &i32| x < q;
        let pred_ngt = |x: &i32, q: &i32| x <= q;

        assert_eq!(arr.partition_point_(&0, pred_lt), 0);
        assert_eq!(arr.partition_point_(&10, pred_lt), 0);
        assert_eq!(arr.partition_point_(&20, pred_lt), 1);
        assert_eq!(arr.partition_point_(&30, pred_lt), 2);
        assert_eq!(arr.partition_point_(&40, pred_lt), 3);
        assert_eq!(arr.partition_point_(&50, pred_lt), 4);
        assert_eq!(arr.partition_point_(&60, pred_lt), 5);

        // partition_point 可以表达“<= query”
        assert_eq!(arr.partition_point_(&30, &pred_ngt), 3);
        assert_eq!(arr.partition_point_(&50, &pred_ngt), 5);
    }

    #[test]
    fn test_lower_bound_by() {
        let mut arr = OrderedArray::<8, i32>::new();

        for x in [10, 20, 30, 40, 50] {
            assert!(arr.try_insert_ord(x).is_succ());
        }

        let cmp = OrdComparer::new();

        // 第一个 >= query
        assert_eq!(arr.lower_bound_by(&0, &cmp), 0);
        assert_eq!(arr.lower_bound_by(&10, &cmp), 0);
        assert_eq!(arr.lower_bound_by(&15, &cmp), 1);
        assert_eq!(arr.lower_bound_by(&20, &cmp), 1);
        assert_eq!(arr.lower_bound_by(&25, &cmp), 2);
        assert_eq!(arr.lower_bound_by(&30, &cmp), 2);
        assert_eq!(arr.lower_bound_by(&45, &cmp), 4);
        assert_eq!(arr.lower_bound_by(&50, &cmp), 4);
        assert_eq!(arr.lower_bound_by(&60, &cmp), 5);
    }

    #[test]
    fn test_upper_bound_by() {
        let mut arr = OrderedArray::<8, i32>::new();

        for x in [10, 20, 30, 40, 50] {
            assert!(arr.try_insert_ord(x).is_succ());
        }

        let cmp = OrdComparer::new();

        // 第一个 > query
        assert_eq!(arr.upper_bound_by(&0, &cmp), 0);
        assert_eq!(arr.upper_bound_by(&10, &cmp), 1);
        assert_eq!(arr.upper_bound_by(&15, &cmp), 1);
        assert_eq!(arr.upper_bound_by(&20, &cmp), 2);
        assert_eq!(arr.upper_bound_by(&25, &cmp), 2);
        assert_eq!(arr.upper_bound_by(&30, &cmp), 3);
        assert_eq!(arr.upper_bound_by(&45, &cmp), 4);
        assert_eq!(arr.upper_bound_by(&50, &cmp), 5);
        assert_eq!(arr.upper_bound_by(&60, &cmp), 5);
    }

    #[test]
    fn test_binary_search_by() {
        let mut arr = OrderedArray::<8, i32>::new();

        for x in [10, 20, 30, 40, 50] {
            assert!(arr.try_insert_ord(x).is_succ());
        }

        let cmp = OrdComparer::new();

        // 找到
        assert_eq!(arr.binary_search_by(&10, &cmp), Ok(0));
        assert_eq!(arr.binary_search_by(&20, &cmp), Ok(1));
        assert_eq!(arr.binary_search_by(&30, &cmp), Ok(2));
        assert_eq!(arr.binary_search_by(&40, &cmp), Ok(3));
        assert_eq!(arr.binary_search_by(&50, &cmp), Ok(4));

        // 找不到，同时返回 lower_bound / 插入位置
        assert_eq!(arr.binary_search_by(&0, &cmp), Err(0));
        assert_eq!(arr.binary_search_by(&15, &cmp), Err(1));
        assert_eq!(arr.binary_search_by(&25, &cmp), Err(2));
        assert_eq!(arr.binary_search_by(&35, &cmp), Err(3));
        assert_eq!(arr.binary_search_by(&45, &cmp), Err(4));
        assert_eq!(arr.binary_search_by(&60, &cmp), Err(5));
    }

    #[test]
    fn test_binary_search_matches_lower_bound() {
        let mut arr = OrderedArray::<8, i32>::new();

        for x in [10, 20, 30, 40, 50] {
            assert!(arr.try_insert_ord(x).is_succ());
        }

        let cmp = OrdComparer::new();

        for query in [-10, 0, 10, 15, 20, 25, 30, 35, 40, 45, 50, 60] {
            let lower = arr.lower_bound_by(&query, &cmp);

            match arr.binary_search_by(&query, &cmp) {
                Ok(pos) => {
                    assert_eq!(pos, lower);
                    let (k, _) = arr.get_ref_at(pos).unwrap();
                    assert_eq!(*k, query);
                }
                Err(pos) => {
                    assert_eq!(pos, lower);
                }
            }
        }
    }

    #[test]
    fn test_search_empty() {
        let arr = OrderedArray::<8, i32>::new();
        let cmp = OrdComparer::new();

        assert_eq!(arr.len(), 0);

        assert_eq!(arr.partition_point_(&0, |_, _| true), 0);
        assert_eq!(arr.partition_point_(&0, |_, _| false), 0);

        assert_eq!(arr.lower_bound_by(&10, &cmp), 0);
        assert_eq!(arr.upper_bound_by(&10, &cmp), 0);
        assert_eq!(arr.binary_search_by(&10, &cmp), Err(0));
        assert_eq!(arr.find_by(&10, &cmp), None);
    }
}

#[cfg(test)]
mod tests_heter_search_ {
    use super::*;

    #[derive(Clone, Copy, Debug, Default)]
    struct TupleKeyComparer;

    impl TrComparer<(u64, &'static str), u64> for TupleKeyComparer {
        fn compare(&self, value: &(u64, &'static str), query: &u64) -> Ordering {
            value.0.cmp(query)
        }
    }

    #[test]
    fn test_heterogeneous_search() {
        let mut arr = OrderedArray::<8, (u64, &'static str)>::new();

        // let insert_cmp = OrdComparer::new();

        // 如果这里使用 tuple 的 Ord，只是为了方便建立测试数据。
        assert!(arr.try_insert_ord((10, "ten")).is_succ());
        assert!(arr.try_insert_ord((20, "twenty")).is_succ());
        assert!(arr.try_insert_ord((30, "thirty")).is_succ());
        assert!(arr.try_insert_ord((40, "forty")).is_succ());

        let cmp = TupleKeyComparer;

        assert_eq!(arr.lower_bound_by(&5u64, &cmp), 0);
        assert_eq!(arr.lower_bound_by(&10u64, &cmp), 0);
        assert_eq!(arr.lower_bound_by(&15u64, &cmp), 1);
        assert_eq!(arr.lower_bound_by(&20u64, &cmp), 1);
        assert_eq!(arr.lower_bound_by(&25u64, &cmp), 2);
        assert_eq!(arr.lower_bound_by(&40u64, &cmp), 3);
        assert_eq!(arr.lower_bound_by(&50u64, &cmp), 4);

        assert_eq!(arr.upper_bound_by(&10u64, &cmp), 1);
        assert_eq!(arr.upper_bound_by(&20u64, &cmp), 2);
        assert_eq!(arr.upper_bound_by(&40u64, &cmp), 4);

        assert_eq!(arr.binary_search_by(&20u64, &cmp), Ok(1));

        assert_eq!(arr.binary_search_by(&25u64, &cmp), Err(2));

        assert_eq!(arr.find_by(&30u64, &cmp), Some((&(30, "thirty"), &())));

        assert_eq!(arr.find_by(&35u64, &cmp), None);
    }
}
