//! SlabList 返回给调用者的槽位句柄。

use core::{
    fmt,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

/// 指向 `SlabList<T>` 中某个已初始化槽位的句柄。
///
/// 句柄内部只保存一个 [`NonNull<T>`]，与裸引用等宽，可以复制并保存到
/// 树节点中。对句柄解引用会直接得到槽位中的元素引用，不需要再访问 arena。
///
/// # 安全约定
///
/// `SlotPtr` 是一个底层 arena 句柄，Rust 无法在类型层面约束它的有效期。
/// 使用方必须遵守以下约定：
///
/// - 句柄必须由某个仍然存活的 `SlabList<T>` 的 `insert` 返回。
/// - 句柄对应的槽位不能已经被 `remove`，除非之后又被重新 `insert` 到同一个槽位。
/// - 通过 [`DerefMut`] 取得 `&mut T` 时，必须保证没有其他引用同时访问该元素。
///   由于句柄是 `Copy` 的，复制句柄并不能获得额外的独占权。
/// - `SlabList` 被 drop 后，所有由它产生的句柄都会悬垂。
///
/// 违反上述约定属于调用方的逻辑错误，可能导致未定义行为。
///
/// # Examples
///
/// ```text
/// use crate::slab_list_::{SlabList, SlabListConfig};
/// use std::alloc::Global;
///
/// let mut arena = SlabList::<u64, SlabListConfig<4096>>::new().unwrap();
/// let mut ptr = arena.insert(7, &Global);
///
/// assert_eq!(*ptr, 7);
/// *ptr = 8;
/// assert_eq!(*ptr, 8);
/// arena.clear(&Global);
/// ```
#[repr(transparent)]
pub struct SlotPtr<T> {
    ptr_: NonNull<T>,
}

impl<T> SlotPtr<T> {
    /// 由 arena 内部构造句柄。
    ///
    /// 调用者必须保证 `ptr` 指向某个已经初始化的槽位。
    pub(super) const fn from_raw_(ptr: NonNull<T>) -> Self {
        SlotPtr { ptr_: ptr }
    }

    /// 返回指向元素的裸指针。
    ///
    /// 解引用该指针同样需要满足 [`SlotPtr`] 文档中的安全约定。
    ///
    /// # Examples
    ///
    /// ```text
    /// use crate::slab_list_::{SlabList, SlabListConfig};
    /// use std::alloc::Global;
    ///
    /// let mut arena = SlabList::<u64, SlabListConfig<4096>>::new().unwrap();
    /// let ptr = arena.insert(1, &Global);
    /// assert!(!ptr.as_ptr().is_null());
    /// arena.clear(&Global);
    /// ```
    pub fn as_ptr(&self) -> *mut T {
        self.ptr_.as_ptr()
    }

    /// 返回内部的 [`NonNull<T>`]。
    ///
    /// 仅供 crate 内部的 arena 操作使用。
    pub(super) const fn as_non_null_(&self) -> NonNull<T> {
        self.ptr_
    }

    /// 构造一个悬垂句柄，仅供测试中作为占位值使用。
    ///
    /// 该句柄不能解引用；只用于在节点字段尚未填好时占位。
    #[cfg(test)]
    #[allow(unused)]
    pub(crate) fn dangling_for_test_() -> Self {
        SlotPtr {
            ptr_: NonNull::dangling(),
        }
    }
}

impl<T> Copy for SlotPtr<T> {}

impl<T> Clone for SlotPtr<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> PartialEq for SlotPtr<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.ptr_ == other.ptr_
    }
}

impl<T> Eq for SlotPtr<T> {}

impl<T> fmt::Debug for SlotPtr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SlotPtr").field(&self.ptr_).finish()
    }
}

impl<T> Deref for SlotPtr<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        // SAFETY: `SlotPtr` 的安全约定要求句柄由仍然存活的 arena 返回，
        // 且对应槽位尚未被释放。因此 `ptr_` 指向一个已初始化的 T。
        unsafe { self.ptr_.as_ref() }
    }
}

impl<T> DerefMut for SlotPtr<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: 除 `Deref` 的条件外，`DerefMut` 还要求调用者保证没有
        // 其他引用同时访问该元素。这是 `SlotPtr` 文档中约定的调用方责任。
        unsafe { self.ptr_.as_mut() }
    }
}
