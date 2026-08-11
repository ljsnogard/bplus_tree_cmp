use core::{
    cmp::Ordering,
    mem::MaybeUninit,
    ops::{Index, IndexMut},
    ptr,
};

use crate::{OrdComparer, comparer_::TrComparer};

pub enum TryInsertResult<'a, T> {
    Succ(&'a mut T),

    /// Failed due to array is full.
    Full(T),

    /// Failed due to conflict with an existing item in the array.
    Conflict {
        /// The index of the existing item
        at: usize,
        /// The mut ref to the existing item in the array
        item: &'a mut T,
        /// The conflicting value that failed to insert.
        conflict: T,
    },
}

impl<'a, T> TryInsertResult<'a, T> {
    pub const fn try_succ(&self) -> Option<&T> {
        let TryInsertResult::Succ(x) = self else {
            return Option::None;
        };
        Option::Some(x)
    }

    #[inline]
    pub const fn is_succ(&self) -> bool {
        self.try_succ().is_some()
    }

    pub const fn try_full(&self) -> Option<&T> {
        let TryInsertResult::Full(x) = self else {
            return Option::None;
        };
        Option::Some(x)
    }

    #[inline]
    pub const fn is_full(&self) -> bool {
        self.try_full().is_some()
    }

    pub const fn try_conflict(&self) -> Option<(usize, &T)> {
        let TryInsertResult::Conflict {
            at: idx,
            item: _,
            conflict: t
        } = self else {
            return Option::None;
        };
        Option::Some((*idx, t))
    }

    pub const fn is_conflict(&self) -> bool {
        self.try_conflict().is_some()
    }
}

pub struct OrderedArray<T, const N: usize> {
    elems_: [MaybeUninit<T>; N],
    order_: [usize; N],
    count_: usize,
}

impl<T, const N: usize> OrderedArray<T, N> {
    /// 创建一个空的有序数组。
    pub const fn new() -> Self {
        Self {
            elems_: unsafe { MaybeUninit::uninit().assume_init() },
            order_: [0usize; N],
            count_: 0usize,
        }
    }

    /// 返回是否为空。
    pub const fn is_empty(&self) -> bool {
        self.count_ == 0
    }

    /// 返回当前元素个数。
    pub const fn len(&self) -> usize {
        self.count_
    }

    /// 返回最大容量。
    pub const fn capacity(&self) -> usize {
        N
    }

    pub const fn get_ref_at(&self, i: usize) -> Result<&T, usize> {
        if i < self.count_ {
            let p = self.order_[i];
            Result::Ok(unsafe { self.elems_[p].assume_init_ref() })
        } else {
            Result::Err(self.count_)
        }
    }

    pub const fn get_mut_at(&mut self, i: usize) -> Result<&mut T, usize> {
        if i < self.count_ {
            let p = self.order_[i];
            Result::Ok(unsafe { self.elems_[p].assume_init_mut() })
        } else {
            Result::Err(self.count_)
        }
    }

    /// 返回逻辑顺序的迭代器，产生元素引用。
    pub fn iter(&self) -> Iter<'_, T, N> {
        Iter {
            array: self,
            index: 0,
        }
    }

    /// 使用 T 自身的 Ord 作为 `try_insert_by` 的 comparer 然后调用之。
    pub fn try_insert<'f>(&'f mut self, t: T) -> TryInsertResult<'f, T>
    where
        T: Ord,
    {
        let c = OrdComparer::new();
        self.try_insert_by(t, &c)
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
        t: T,
        c: &TyCmp,
    ) -> TryInsertResult<'f, T>
    where
        TyCmp: TrComparer<T>,
    {
        if self.count_ >= N {
            return TryInsertResult::Full(t)
        }
        // 0. 获得物理写入位置的索引
        let new_phys_idx = self.order_[self.count_];
        if self.count_ == 0 {
            for i in 0..N {
                self.order_[i] = i
            }
            self.elems_[0].write(t);
            self.count_ += 1;
        } else {
            // 1. 在现有逻辑顺序（order[0..len]）中查找插入位置
            // let pos = match self.binary_search_(&t, c) {
            //     Result::Ok(x) => x,
            //     Result::Err(x) => {
            //         let conflict_phys_idx = self.order_[x];
            //         let conflict_item_mut = unsafe {
            //             self.elems_[conflict_phys_idx].assume_init_mut()
            //         };
            //         return TryInsertResult::Conflict {
            //             at: x,
            //             item: conflict_item_mut,
            //             conflict: t,
            //         };
            //     }
            // };
            let curr_count = self.count_;
            let pos = self.lower_bound_by(&t, c);
            let lb_phyx_idx = self.order_[pos];
            let lb = self.get_elem_mut_at_(lb_phyx_idx);
            if pos < curr_count && c.compare(lb, &t) == Ordering::Equal {
                return TryInsertResult::Conflict {
                    at: pos,
                    item: lb,
                    conflict: t,
                };
            }
            // 2. 移动 order 元素以腾出位置（从后往前移动）
            //    新索引 = len（旧长度），插入后总长度变为 len+1
            self.move_insert_position_(pos, new_phys_idx);

            // 3. 将新元素写入物理存储的末尾（索引 = 当前长度）
            self.elems_[new_phys_idx].write(t);
            // 4. 更新长度
            self.count_ += 1;
        }
        TryInsertResult::Succ(unsafe {
            self.elems_[new_phys_idx].assume_init_mut()
        })
    }

    /// 移除最后一个元素，也是唯一有效的移除方式。
    /// 这个方法不会影响下一次插入，
    /// 因为移除后会在最后一个索引位置记录被移除元素的物理索引。
    pub fn try_remove_last(&mut self) -> Option<T> {
        if self.count_ == 0 {
            return Option::None
        }
        let last_phyx_ids = self.order_[self.count_ - 1];
        let x = &mut self.elems_[last_phyx_ids];
        self.count_ -= 1;
        Option::Some(unsafe { x.assume_init_read() })
    }

    /// 查找 comparer 意义下等价的元素。
    ///
    /// 找到时返回元素引用，否则返回 `None`。
    pub fn find_by<Q, C>(&self, query: &Q, cmp: &C) -> Option<&T>
    where
        C: TrComparer<T, Q>,
    {
        if let Result::Ok(p) = self.binary_search_by(query, cmp) {
            self.get_ref_at(p).ok()
        } else {
            Option::None
        }
    }

    /// 使用 Comparer 对有序数组执行二分查找。
    ///
    /// 找到 comparer 意义下等价的元素时返回 `Ok(index)`；
    /// 否则返回 `Err(index)`，其中 `index` 是该元素应该插入的位置。
    pub fn binary_search_by<Q, C>(
        &self,
        query: &Q,
        cmp: &C,
    ) -> Result<usize, usize>
    where
        C: TrComparer<T, Q>,
    {
        let pos = self.lower_bound_by(query, cmp);
        let lb_phyx_idx = self.order_[pos];
        let lb = self.get_elem_at_(lb_phyx_idx);
        if pos < self.count_ && cmp.compare(lb, query) == Ordering::Equal {
            Ok(pos)
        } else {
            Err(pos)
        }
    }

    /// 寻找第一个满足 elem >= query 的位置
    pub fn lower_bound_by<Q, C>(&self, query: &Q, cmp: &C) -> usize
    where
        C: TrComparer<T, Q>,
    {
        self.partition_point_(|elem| {
            cmp.compare(elem, query) == Ordering::Less
        })
    }

    /// 寻找第一个满足 elem > query 的位置
    pub fn upper_bound_by<Q, C>(&self, query: &Q, cmp: &C) -> usize
    where
        C: TrComparer<T, Q>,
    {
        self.partition_point_(|elem| {
            matches!(
                cmp.compare(elem, query),
                Ordering::Less | Ordering::Equal
            )
        })
    }

    /// 返回 [0, count_) 中第一个使 pred 返回 false 的位置。
    fn partition_point_<P>(&self, mut pred: P) -> usize
    where
        P: FnMut(&T) -> bool,
    {
        let mut left = 0;
        let mut right = self.count_;

        while left < right {
            let mid = left + (right - left) / 2;
            let elem = self.get_elem_at_(self.order_[mid]);

            if pred(elem) {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        left
    }

    // /// 在现有逻辑顺序中查找新物理索引应插入的位置（二分查找）。
    // fn binary_search_<TyHint, TyCmp>(
    //     &self,
    //     hint: &TyHint,
    //     cmp: &TyCmp,
    // ) -> Result<usize, usize>
    // where
    //     TyCmp: TrComparer<T, TyHint>,
    // {
    //     let mut left = 0;
    //     let mut right = self.count_; // 当前有效逻辑长度
    //     while left < right {
    //         let mid = (left + right) / 2;
    //         let mid_phys = self.order_[mid];
    //         let mid_val = self.get_elem_at_(mid_phys);
    //         let ordering = cmp.compare(mid_val, hint);
    //         match ordering {
    //             Ordering::Less => left = mid + 1,
    //             Ordering::Greater => right = mid,
    //             Ordering::Equal => return Result::Err(mid),
    //         }
    //     }
    //     Result::Ok(left)
    // }

    fn move_insert_position_(&mut self, i: usize, p: usize) {
        for i in (i..self.count_).rev() {
            self.order_[i + 1] = self.order_[i];
        }
        self.order_[i] = p;
    }

    fn get_elem_at_(&self, p: usize) -> &T {
        unsafe { self.elems_[p].assume_init_ref() }
    }

    fn get_elem_mut_at_(&mut self, p: usize) -> &mut T {
        unsafe { self.elems_[p].assume_init_mut() }
    }
}

impl<T, const N: usize> OrderedArray<T, N>
where
    T: Ord,
{
    pub const fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Default for OrderedArray<T, N> {
    fn default() -> Self {
        OrderedArray::new()
    }
}

/// 实现 Drop，正确释放已初始化的元素。
impl<T, const N: usize> Drop for OrderedArray<T, N> {
    fn drop(&mut self) {
        for i in 0..self.count_ {
            let phys_idx = self.order_[i];
            unsafe {
                ptr::drop_in_place(self.elems_[phys_idx].as_mut_ptr());
            }
        }
    }
}

impl<T, const N: usize> Index<usize> for OrderedArray<T, N> {
    type Output = T;
    
    fn index(&self, index: usize) -> &Self::Output {
        self.get_ref_at(index).expect("invalid index: {index}")
    }
}

impl<T, const N: usize> IndexMut<usize> for OrderedArray<T, N> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.get_mut_at(index).expect("invalid index: {index}")
    }
}

/// 迭代器：按逻辑顺序遍历元素。
pub struct Iter<'a, T, const N: usize> {
    array: &'a OrderedArray<T, N>,
    index: usize,
}

impl<'a, T, const N: usize> Iterator for Iter<'a, T, N> {
    type Item = &'a T;

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

#[cfg(test)]
mod tests {
    use std::{
        vec,
        vec::Vec,
        string::{String, ToString},
    };

    use super::*;

    #[test]
    fn test_insert_and_order() {
        // let c = OrdComparer::default();
        let mut arr = OrderedArray::<i32, 5>::default();
        assert!(arr.try_insert(3).is_succ());
        assert!(arr.try_insert(1).is_succ());
        assert!(arr.try_insert(4).is_succ());
        assert!(arr.try_insert(2).is_succ());

        let conflict = arr.try_insert(1);
        assert!(conflict.is_conflict());
        let TryInsertResult::Conflict {
            at: idx,
            item,
            conflict: x,
        } = conflict else {
            unreachable!()
        };
        assert_eq!(idx, 0usize);
        assert_eq!(*item, 1);
        assert_eq!(x, 1);

        let expected = [1, 2, 3, 4];
        for (u, i) in arr.iter().enumerate() {
            assert_eq!(*i, expected[u])
        }
    }

    #[test]
    fn test_full() {
        // let c = OrdComparer::default();
        let mut arr = OrderedArray::<i32, 2>::default();
        assert!(arr.try_insert(10).is_succ());
        assert!(arr.try_insert(20).is_succ());
        assert!(arr.try_insert(30).is_full());
    }

    #[test]
    fn test_get() {
        // let c = OrdComparer::default();
        let mut arr = OrderedArray::<String, 4>::default();
        assert!(arr.try_insert("b".to_string()).is_succ());
        assert!(arr.try_insert("a".to_string()).is_succ());
        assert!(arr.try_insert("c".to_string()).is_succ());
        assert_eq!(arr.get_ref_at(0).map(String::as_str), Ok("a"));
        assert_eq!(arr.get_ref_at(1).map(String::as_str), Ok("b"));
        assert_eq!(arr.get_ref_at(2).map(String::as_str), Ok("c"));
        assert!(arr.get_ref_at(3).is_err());
    }

    #[test]
    fn test_remove_last() {
        let mut arr = OrderedArray::<i32, 10>::default();
        // let cmp = OrdComparer::new();

        arr.try_insert(5);
        arr.try_insert(2);
        arr.try_insert(8);
        arr.try_insert(1); // 当前顺序 [1, 2, 5, 8]

        let removed = arr.try_remove_last();
        assert_eq!(removed, Some(8));
        assert_eq!(arr.len(), 3);

        let collected: Vec<i32> = arr.iter().copied().collect();
        assert_eq!(collected, vec![1, 2, 5]);

        // 再次删除
        let removed2 = arr.try_remove_last();
        assert_eq!(removed2, Some(5));
        assert_eq!(arr.len(), 2);
        let collected2: Vec<i32> = arr.iter().copied().collect();
        assert_eq!(collected2, vec![1, 2]);
    }

    #[test]
    fn test_remove_then_insert() {
        let mut arr = OrderedArray::<i32, 10>::default();
        // let cmp = OrdComparer::new();

        // 插入 [3, 1, 4, 5] → 逻辑顺序 [1, 3, 4, 5]
        arr.try_insert(3);
        arr.try_insert(1);
        arr.try_insert(4);
        arr.try_insert(5);
        assert_eq!(arr.len(), 4);

        // 删除最大值 5
        assert_eq!(arr.try_remove_last(), Some(5));
        assert_eq!(arr.len(), 3);
        // 当前逻辑顺序 [1, 3, 4]

        // 插入 2 → 期望 [1, 2, 3, 4]
        arr.try_insert(2);
        let collected: Vec<i32> = arr.iter().copied().collect();
        assert_eq!(collected, vec![1, 2, 3, 4]);

        // 删除最大值 4
        assert_eq!(arr.try_remove_last(), Some(4));
        // 插入 6 → 期望 [1, 2, 3, 6]
        arr.try_insert(6);
        let collected: Vec<i32> = arr.iter().copied().collect();
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
            let mut arr = OrderedArray::<Arc<usize>, 10>::new();
            // let cmp = ArcCmp;
            let mut weak_vec = Vec::with_capacity(10);

            for i in 0..10 {
                let arc = Arc::new(i);
                weak_vec.push(Arc::downgrade(&arc)); // 插入前获取 Weak
                let res = arr.try_insert_by(arc, &cmp);
                assert!(matches!(res, TryInsertResult::Succ(_)));
            }
            // arr 在此作用域结束时被 drop
            weak_vec
        };

        // 所有弱引用应该已失效（因为 Arc 已被释放）
        for (i, w) in weaks.iter().enumerate() {
            assert!(
                w.upgrade().is_none(),
                "Weak at index {} should be dead",
                i
            );
        }
    }
}

#[cfg(test)]
mod tests_search_ {
    use super::*;

    #[test]
    fn test_partition_point() {
        let mut arr = OrderedArray::<i32, 8>::new();

        for x in [10, 20, 30, 40, 50] {
            assert!(arr.try_insert(x).is_succ());
        }

        // [10, 20, 30, 40, 50]

        assert_eq!(arr.partition_point_(|x| *x < 0), 0);
        assert_eq!(arr.partition_point_(|x| *x < 10), 0);
        assert_eq!(arr.partition_point_(|x| *x < 20), 1);
        assert_eq!(arr.partition_point_(|x| *x < 30), 2);
        assert_eq!(arr.partition_point_(|x| *x < 40), 3);
        assert_eq!(arr.partition_point_(|x| *x < 50), 4);
        assert_eq!(arr.partition_point_(|x| *x < 60), 5);

        // partition_point 可以表达“<= query”
        assert_eq!(arr.partition_point_(|x| *x <= 30), 3);
        assert_eq!(arr.partition_point_(|x| *x <= 50), 5);
    }

    #[test]
    fn test_lower_bound_by() {
        let mut arr = OrderedArray::<i32, 8>::new();

        for x in [10, 20, 30, 40, 50] {
            assert!(arr.try_insert(x).is_succ());
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
        let mut arr = OrderedArray::<i32, 8>::new();

        for x in [10, 20, 30, 40, 50] {
            assert!(arr.try_insert(x).is_succ());
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
        let mut arr = OrderedArray::<i32, 8>::new();

        for x in [10, 20, 30, 40, 50] {
            assert!(arr.try_insert(x).is_succ());
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
        let mut arr = OrderedArray::<i32, 8>::new();

        for x in [10, 20, 30, 40, 50] {
            assert!(arr.try_insert(x).is_succ());
        }

        let cmp = OrdComparer::new();

        for query in [-10, 0, 10, 15, 20, 25, 30, 35, 40, 45, 50, 60] {
            let lower = arr.lower_bound_by(&query, &cmp);

            match arr.binary_search_by(&query, &cmp) {
                Ok(pos) => {
                    assert_eq!(pos, lower);
                    assert_eq!(*arr.get_ref_at(pos).unwrap(), query);
                }
                Err(pos) => {
                    assert_eq!(pos, lower);
                }
            }
        }
    }

    #[test]
    fn test_search_empty() {
        let arr = OrderedArray::<i32, 8>::new();
        let cmp = OrdComparer::new();

        assert_eq!(arr.len(), 0);

        assert_eq!(arr.partition_point_(|_| true), 0);
        assert_eq!(arr.partition_point_(|_| false), 0);

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
        fn compare(
            &self,
            value: &(u64, &'static str),
            query: &u64,
        ) -> Ordering {
            value.0.cmp(query)
        }
    }

    #[test]
    fn test_heterogeneous_search() {
        let mut arr = OrderedArray::<(u64, &'static str), 8>::new();

        // let insert_cmp = OrdComparer::new();

        // 如果这里使用 tuple 的 Ord，只是为了方便建立测试数据。
        assert!(arr.try_insert((10, "ten")).is_succ());
        assert!(arr.try_insert((20, "twenty")).is_succ());
        assert!(arr.try_insert((30, "thirty")).is_succ());
        assert!(arr.try_insert((40, "forty")).is_succ());

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

        assert_eq!(
            arr.binary_search_by(&20u64, &cmp),
            Ok(1)
        );

        assert_eq!(
            arr.binary_search_by(&25u64, &cmp),
            Err(2)
        );

        assert_eq!(
            arr.find_by(&30u64, &cmp),
            Some(&(30, "thirty"))
        );

        assert_eq!(
            arr.find_by(&35u64, &cmp),
            None
        );
    }
}