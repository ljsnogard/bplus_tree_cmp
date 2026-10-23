use crate::{
    node_::{DataLeaf, IndexNode, RootNode, TreeNode, TreeNodePtr},
    slab_list_::SlotPtr,
};

#[derive(Clone, Copy, Debug)]
pub enum PartitionPoint<const M: usize, K, V> {
    Data(SlotPtr<(K, V)>),
    Node(TreeNodePtr<M, K, V>),
}

/// 从某一个给定的节点开始进行递归搜索，返回第一个使 pred 返回 false 的位置。
/// 且该位置应为 DataLeaf （如果非空）或者 node_idx （如果空节点，例如当
/// 树刚刚构建，完全没有任何节点没有 Root 的情况下）
/// Pred 的语义应该
pub fn partition_point_recursive_<'f, const M: usize, K, V, H, P>(
    node_idx: &'f TreeNodePtr<M, K, V>,
    hint: &'f H,
    pred: P,
) -> PartitionPoint<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    let node = &**node_idx;
    match node {
        TreeNode::Index(index) => partition_point_from_index_(index, hint, pred),
        TreeNode::Leaf(leaf) => partition_point_from_leaf(leaf, hint, pred),
        TreeNode::Root(root) => partition_point_from_root(root, node_idx, hint, pred),
    }
}

/// 从 IndexNode 开始进行递归搜索，返回第一个使 pred 返回 false 的位置。
/// 且该位置应为 DataLeaf （如果非空）或者 node_idx （如果空节点）
fn partition_point_from_index_<'f, const M: usize, K, V, H, P>(
    index: &'f IndexNode<M, K, V>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPoint<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    todo!()
}

fn partition_point_from_leaf<'f, const M: usize, K, V, H, P>(
    leaf: &'f DataLeaf<M, K, V>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPoint<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    todo!()
}

fn partition_point_from_root<'f, const M: usize, K, V, H, P>(
    root: &'f RootNode<M, K, V>,
    root_idx: &'f TreeNodePtr<M, K, V>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPoint<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    todo!()
}

#[cfg(test)]
mod tests_search_tree_ {
    use alloc::alloc::Global;

    use super::*;
    use crate::{
        comparer_::{HomoSlotComparer, TrComparer},
        node_::{
            DataLeaf, DataPtrOrdArr, IndexNode, IndexPtrOrdArr, LeafSentinel, RootNode, TreeNode,
        },
        ordered_arr_::OrderedArray,
        slab_list_::{SlabList, SlotPtr},
    };

    const M: usize = 4;

    type KeyI32 = i32;
    type ValStr = &'static str;
    type Tree = TreeNode<M, KeyI32, ValStr>;
    type NodePtr = TreeNodePtr<M, KeyI32, ValStr>;
    type DataPtr = SlotPtr<(KeyI32, ValStr)>;

    struct SearchFixture {
        data_arena: SlabList<(KeyI32, ValStr)>,
        node_arena: SlabList<Tree>,
        root: NodePtr,

        // 保存真实的 data 句柄，方便测试时验证返回的 DataPtr。
        data: [DataPtr; 12],

        // 保存叶节点 ID，方便直接验证 leaf chain。
        leaves: [NodePtr; 6],

        // 保存 index 节点 ID。
        indexes: [NodePtr; 3],
    }

    struct KvPairComparer;

    impl TrComparer<(KeyI32, ValStr)> for KvPairComparer {
        fn compare(&self, a: &(KeyI32, ValStr), b: &(KeyI32, ValStr)) -> core::cmp::Ordering {
            i32::cmp(&a.0, &b.0)
        }
    }

    type DataPtrComparer<'f> = HomoSlotComparer<'f, (KeyI32, ValStr), KvPairComparer>;

    impl SearchFixture {
        fn new() -> Self {
            let mut data_arena = SlabList::<(KeyI32, ValStr)>::new(Global).expect("配置应当有效");
            let mut node_arena = SlabList::<Tree>::new(Global).expect("配置应当有效");

            /*
             * ---------------------------------------------------------
             * 1. 建立 Data SlabList
             *
             * 数据只存在这里一次。
             * IndexNode 和 DataLeaf 都只保存 SlotPtr<(K,V)>。
             * ---------------------------------------------------------
             */

            let data = [
                data_arena.insert((10, "ten")),
                data_arena.insert((20, "twenty")),
                data_arena.insert((30, "thirty")),
                data_arena.insert((40, "forty")),
                data_arena.insert((50, "fifty")),
                data_arena.insert((60, "sixty")),
                data_arena.insert((70, "seventy")),
                data_arena.insert((80, "eighty")),
                data_arena.insert((90, "ninety")),
                data_arena.insert((100, "one hundred")),
                data_arena.insert((110, "one hundred ten")),
                data_arena.insert((120, "one hundred twenty")),
            ];

            /*
             * ---------------------------------------------------------
             * 2. 先建立六个 Leaf
             *
             * 这里暂时使用 dummy id 填 prev/next。
             * 因为 TreeNodePtr 是 Copy 的，所以之后可以重新建立
             * DataLeaf 来完成 sibling link。
             *
             * 更简单的做法是先插入叶子，再用 DerefMut 修改 link。
             * ---------------------------------------------------------
             */

            let dummy = {
                // 只用于初始化字段，之后不会被真正使用。
                node_arena.insert(TreeNode::Leaf(DataLeaf {
                    order_arr_: OrderedArray::new(),
                    prev_sibl_: SlotPtr::dangling_for_test_(),
                    next_sibl_: SlotPtr::dangling_for_test_(),
                }))
            };

            let mut leaves = [dummy, dummy, dummy, dummy, dummy, dummy];

            let kv_cmp = KvPairComparer;
            let ptr_cmp = DataPtrComparer::new(&kv_cmp);

            for i in 0..6 {
                let mut leaf_arr = DataPtrOrdArr::new();

                let begin = i * 2;

                assert!(leaf_arr.try_insert_item(data[begin], &ptr_cmp,).is_succ());

                assert!(
                    leaf_arr
                        .try_insert_item(data[begin + 1], &ptr_cmp,)
                        .is_succ()
                );

                leaves[i] = node_arena.insert(TreeNode::Leaf(DataLeaf {
                    order_arr_: leaf_arr,
                    prev_sibl_: dummy,
                    next_sibl_: dummy,
                }));
            }

            /*
             * dummy 不再需要作为树的一部分。
             *
             * 注意：dummy 节点仍然存在于 SlabList 中，但不会被 Root /
             * Index / Leaf 引用。
             *
             * 对 search 测试而言这没有任何影响。
             */

            /*
             * ---------------------------------------------------------
             * 3. 修复 Leaf sibling links
             * ---------------------------------------------------------
             */

            for i in 0..6 {
                let prev = if i == 0 { leaves[i] } else { leaves[i - 1] };

                let next = if i == 5 { leaves[i] } else { leaves[i + 1] };

                let TreeNode::Leaf(leaf) = &mut *leaves[i] else {
                    unreachable!()
                };

                leaf.prev_sibl_ = prev;
                leaf.next_sibl_ = next;
            }

            /*
             * ---------------------------------------------------------
             * 4. 建立三个 IndexNode
             *
             * 每个 entry：
             *
             *     separator key -> child
             *
             * separator 是 child subtree 的最大 key。
             *
             * I0:
             *     20 -> L0
             *     40 -> L1
             *
             * I1:
             *     60 -> L2
             *     80 -> L3
             *
             * I2:
             *     100 -> L4
             *     120 -> L5
             * ---------------------------------------------------------
             */

            let mut indexes = [dummy, dummy, dummy];

            let index_ranges = [
                (0usize, 20, 0usize, 40, 1usize),
                (2usize, 60, 2usize, 80, 3usize),
                (4usize, 100, 4usize, 120, 5usize),
            ];

            for (i, &(data0, _, leaf0, data1, leaf1)) in index_ranges.iter().enumerate() {
                let mut children = IndexPtrOrdArr::new();

                assert!(
                    children
                        .try_insert_pair_by((data[data0], leaves[leaf0]), &ptr_cmp,)
                        .is_succ()
                );

                assert!(
                    children
                        .try_insert_pair_by((data[data0 + 1], leaves[leaf0]), &ptr_cmp,)
                        .is_succ()
                );

                /*
                 * 上面两项只是为了说明：
                 *
                 * IndexNode 的 entry 应该和 child 建立明确对应关系。
                 *
                 * 实际 fixture 最终采用：
                 *
                 *     max(child) -> child
                 *
                 * 所以重新构造正确的 children。
                 */

                let mut children = IndexPtrOrdArr::new();

                assert!(
                    children
                        .try_insert_pair_by((data[data0 + 1], leaves[leaf0]), &ptr_cmp,)
                        .is_succ()
                );

                // assert!(
                //     children
                //         .try_insert_pair_by(
                //             (data1, leaves[leaf1]),
                //             &ptr_cmp,
                //         )
                //         .is_succ()
                // );

                /*
                 * parent 在 Root 建立以后再填写。
                 * 当前先使用 dummy。
                 */

                indexes[i] = node_arena.insert(TreeNode::Index(IndexNode {
                    children_: children,
                    parent_: dummy,
                }));
            }

            /*
             * ---------------------------------------------------------
             * 5. 建立 Root
             *
             * Root 不需要 separator key。
             *
             * Root 只保存：
             *
             *     I0
             *     I1
             *     I2
             *
             * ---------------------------------------------------------
             */

            let mut root_children = IndexPtrOrdArr::new();

            // for index in indexes {
            //     assert!(
            //         root_children
            //             .try_insert_pair_by(
            //                 (index, ),
            //                 &hetero_cmp,
            //             )
            //             .is_succ()
            //     );
            // }

            let root = node_arena.insert(TreeNode::Root(RootNode {
                children_: root_children,
                sentinel_: Some(LeafSentinel {
                    head_: leaves[0],
                    tail_: leaves[5],
                }),
            }));

            /*
             * ---------------------------------------------------------
             * 6. 修复 IndexNode::parent_
             * ---------------------------------------------------------
             */

            // for index in indexes {
            //     let TreeNode::Index(node) =
            //         index.get_mut(&mut node_arena)
            //     else {
            //         unreachable!()
            //     };

            //     node.parent_ = root;
            // }

            Self {
                data_arena,
                node_arena,
                root,
                data,
                leaves,
                indexes,
            }
        }
    }
}
