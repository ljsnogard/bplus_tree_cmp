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

    pub fn search_by<'f, TyHint, TyCmp>(
        &'f self,
        hint: &TyHint,
        cmp: &TyCmp,
    ) -> Result<&'f T, Option<&'f T>>
    where
        TyCmp: TrComparer<T, TyHint>,
    {
        if self.count_ == 0 {
            return Result::Err(Option::None);
        } 
        self.binary_search_(hint, cmp)
            .map(|x| self.get_elem_at_(x))
            .map_err(|u| Option::Some(self.get_elem_at_(u)))
    }

    pub fn try_insert<'f>(&'f mut self, t: T) -> TryInsertResult<'f, T>
    where
        T: Ord,
    {
        let c = OrdComparer::new();
        self.try_insert_by(t, &c)
    }

    /// 插入一个新元素，物理上追加至末尾，并更新逻辑顺序。
    /// 若数组已满，返回 `Err(new_value)`。
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
            let pos = match self.binary_search_(&t, c) {
                Result::Ok(x) => x,
                Result::Err(x) => {
                    let conflict_phys_idx = self.order_[x];
                    let conflict_item_mut = unsafe {
                        self.elems_[conflict_phys_idx].assume_init_mut()
                    };
                    return TryInsertResult::Conflict {
                        at: x,
                        item: conflict_item_mut,
                        conflict: t,
                    };
                }
            };

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

    /// 在现有逻辑顺序中查找新物理索引应插入的位置（二分查找）。
    fn binary_search_<TyHint, TyCmp>(
        &self,
        hint: &TyHint,
        cmp: &TyCmp,
    ) -> Result<usize, usize>
    where
        TyCmp: TrComparer<T, TyHint>,
    {
        let mut left = 0;
        let mut right = self.count_; // 当前有效逻辑长度
        while left < right {
            let mid = (left + right) / 2;
            let mid_phys = self.order_[mid];
            let mid_val = self.get_elem_at_(mid_phys);
            let ordering = cmp.compare(mid_val, hint);
            match ordering {
                Ordering::Less => left = mid + 1,
                Ordering::Greater => right = mid,
                Ordering::Equal => return Result::Err(mid),
            }
        }
        Result::Ok(left)
    }

    fn move_insert_position_(&mut self, i: usize, p: usize) {
        for i in (i..self.count_).rev() {
            self.order_[i + 1] = self.order_[i];
        }
        self.order_[i] = p;
    }

    fn get_elem_at_<'f>(&'f self, p: usize) -> &'f T {
        unsafe { self.elems_[p].assume_init_ref() }
    }

    fn get_elem_mut_at_<'f>(&'f mut self, p: usize) -> &'f mut T {
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