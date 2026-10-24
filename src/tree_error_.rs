//! 树构造错误。

use core::fmt;

use crate::slab_list_::SlabListError;

/// 构造树时可能返回的错误。
///
/// 目前错误来源是底层 `SlabList` 的 Chunk 配置无法容纳树的节点类型。
/// `SlabListError` 保持 crate 内部，调用方通过本类型的 [`core::fmt::Display`] 输出
/// 获取具体原因。
///
/// # Examples
///
/// ```
/// use bplustree_cmp::{BPlusTree, TreeError};
///
/// let result: Result<BPlusTree<4, _, u64, u64>, TreeError> =
///     BPlusTree::new();
/// assert!(result.is_ok());
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeError {
    /// 底层 SlabList 错误；不能对外暴露其具体类型。
    inner_: SlabListError,
}

impl TreeError {
    /// 由内部 SlabList 错误构造。
    pub(crate) const fn from_slab_(inner: SlabListError) -> Self {
        TreeError { inner_: inner }
    }
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner_.fmt(f)
    }
}

impl core::error::Error for TreeError {}

#[cfg(test)]
mod tests {
    use std::string::ToString;

    use super::*;

    /// 测试目标：验证 TreeError 能包装内部 SlabList 错误并保留可读信息。
    /// Test goal: Verify that TreeError wraps internal SlabList errors while
    /// preserving readable diagnostics.
    ///
    /// 测试手段：用 `SlabListError::ChunkTooSmall` 构造 TreeError，检查相等性与
    /// Display 输出。
    /// Test method: Construct a TreeError from `SlabListError::ChunkTooSmall` and
    /// check equality and Display output.
    ///
    /// 判定标准：相同内部错误的 TreeError 相等，Display 输出与内部错误一致。
    /// Assertions: TreeErrors with the same inner error are equal, and Display
    /// matches the inner error's message.
    #[test]
    fn test_tree_error_wraps_slab_error() {
        let error = TreeError::from_slab_(SlabListError::ChunkTooSmall);
        assert_eq!(error, TreeError::from_slab_(SlabListError::ChunkTooSmall));
        assert_eq!(error.to_string(), SlabListError::ChunkTooSmall.to_string());
    }
}
