use crate::{
    arena_::{Arena, Idx},
    node_::{DataLeaf, IndexNode, RootNode, TreeNode, TreeNodeId},
};

#[derive(Clone, Copy, Debug)]
pub enum PartitionPointIdx<const M: usize, K, V> {
    Data(Idx<(K, V)>),
    Node(TreeNodeId<M, K, V>),
}

/// 从某一个给定的节点开始进行递归搜索，返回第一个使 pred 返回 false 的位置。
/// 且该位置应为 DataLeaf （如果非空）或者 node_idx （如果空节点，例如当
/// 树刚刚构建，完全没有任何节点没有 Root 的情况下）
/// Pred 的语义应该
pub fn partition_point_recursive_<'f, const M: usize, K, V, H, P>(
    node_idx: &'f TreeNodeId<M, K, V>,
    arena: &'f Arena<TreeNode<M, K, V>>,
    hint: &'f H,
    pred: P,
) -> PartitionPointIdx<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    let node = node_idx.get(arena);
    match node {
        TreeNode::Index(index) => partition_point_from_index_(index, arena, hint, pred),
        TreeNode::Leaf(leaf) => partition_point_from_leaf(leaf, arena, hint, pred),
        TreeNode::Root(root) => partition_point_from_root(root, node_idx, arena, hint, pred),
    }
}

/// 从 IndexNode 开始进行递归搜索，返回第一个使 pred 返回 false 的位置。
/// 且该位置应为 DataLeaf （如果非空）或者 node_idx （如果空节点）
fn partition_point_from_index_<'f, const M: usize, K, V, H, P>(
    index: &'f IndexNode<M, K, V>,
    arena: &'f Arena<TreeNode<M, K, V>>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPointIdx<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    todo!()
}

fn partition_point_from_leaf<'f, const M: usize, K, V, H, P>(
    leaf: &'f DataLeaf<M, K, V>,
    arena: &'f Arena<TreeNode<M, K, V>>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPointIdx<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    todo!()
}

fn partition_point_from_root<'f, const M: usize, K, V, H, P>(
    root: &'f RootNode<M, K, V>,
    root_idx: &'f TreeNodeId<M, K, V>,
    arena: &'f Arena<TreeNode<M, K, V>>,
    hint: &'f H,
    mut pred: P,
) -> PartitionPointIdx<M, K, V>
where
    P: FnMut(&K, &H) -> bool,
{
    todo!()
}

#[cfg(test)]
mod tests_search_tree_ {
    use super::*;
    use crate::{
        arena_::{Arena, HeteroIdxComparer, HomoIdxComparer},
        comparer_::TrComparer,
        node_::{
            DataIdOrdArr, IndexIdOrdArr, DataLeaf, IndexNode, IndexOrdKeyComparer, LeafSentinel, RootNode,
            TreeNode,
        },
        ordered_arr_::OrderedArray,
    };

    const M: usize = 4;

    type KeyI32 = i32;
    type ValStr = &'static str;
    type Tree = TreeNode<M, KeyI32, ValStr>;
    type NodeId = TreeNodeId<M, KeyI32, ValStr>;
    type DataId = Idx<(KeyI32, ValStr)>;

    struct SearchFixture {
        data_arena: Arena<(KeyI32, ValStr)>,
        node_arena: Arena<Tree>,
        root: NodeId,

        // 保存真实的 data id，方便测试时验证返回的 DataId。
        data: [DataId; 12],

        // 保存叶节点 ID，方便直接验证 leaf chain。
        leaves: [NodeId; 6],

        // 保存 index 节点 ID。
        indexes: [NodeId; 3],
    }

    struct KvPairComparer;

    impl TrComparer<(KeyI32, ValStr)> for KvPairComparer {
        fn compare(&self, a: &(KeyI32, ValStr), b: &(KeyI32, ValStr)) -> core::cmp::Ordering {
            i32::cmp(&a.0, &b.0)
        }
    }

    type DataIdComparer<'f> = HomoIdxComparer<'f, (KeyI32, ValStr), KvPairComparer>;

    impl SearchFixture {
        fn new() -> Self {
            let mut data_arena = Arena::<(KeyI32, ValStr)>::new();
            let mut node_arena = Arena::<Tree>::new();

            // ---------------------------------------------------------
            // 1. 建立 Data Arena
            //
            // 数据只存在这里一次。
            // IndexNode 和 DataLeaf 都只保存 Idx<(K,V)>。
            // ---------------------------------------------------------

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

            // ---------------------------------------------------------
            // 2. 先建立六个 Leaf
            //
            // 这里暂时使用 dummy id 填 prev/next。
            // 因为 TreeNodeId 是 Copy 的，所以之后可以重新建立
            // DataLeaf 来完成 sibling link。
            // ---------------------------------------------------------

            let dummy = {
                // 只用于初始化字段，之后不会被真正使用。
                // 此处需要一个合法 NodeId，因此先插入一个临时节点。
                node_arena.insert(TreeNode::Leaf(DataLeaf {
                    order_arr_: OrderedArray::new(),
                    prev_sibl_: Idx::new_for_test_(0),
                    next_sibl_: Idx::new_for_test_(0),
                }))
            };

            let mut leaves = [dummy, dummy, dummy, dummy, dummy, dummy];

            let kv_cmp = KvPairComparer;
            let idx_cmp = DataIdComparer::new(&kv_cmp, &data_arena);

            for i in 0..6 {
                let mut leaf_arr = DataIdOrdArr::new();

                let begin = i * 2;

                assert!(leaf_arr.try_insert_item(data[begin], &idx_cmp,).is_succ());

                assert!(
                    leaf_arr
                        .try_insert_item(data[begin + 1], &idx_cmp,)
                        .is_succ()
                );

                leaves[i] = node_arena.insert(TreeNode::Leaf(DataLeaf {
                    order_arr_: leaf_arr,
                    prev_sibl_: dummy,
                    next_sibl_: dummy,
                }));
            }

            // dummy 不再需要作为树的一部分。Arena 目前不支持移除，这里保留但不会被引用。

            // ---------------------------------------------------------
            // 3. 修复 Leaf sibling links
            // ---------------------------------------------------------

            for i in 0..6 {
                let prev = if i == 0 { leaves[i] } else { leaves[i - 1] };
                let next = if i == 5 { leaves[i] } else { leaves[i + 1] };

                let TreeNode::Leaf(leaf) = leaves[i].get_mut(&mut node_arena) else {
                    unreachable!()
                };

                leaf.prev_sibl_ = prev;
                leaf.next_sibl_ = next;
            }

            let index_ord_cmp = IndexOrdKeyComparer::pick_first_key(&data_arena);
            let hetero_cmp = HeteroIdxComparer::new(&index_ord_cmp, &data_arena, &node_arena);

            // ---------------------------------------------------------
            // 4. 建立三个 IndexNode
            //
            // 每个 entry：
            //     separator key -> child
            // separator 是 child subtree 的最大 key。
            // I0: 20 -> L0, 40 -> L1
            // I1: 60 -> L2, 80 -> L3
            // I2: 100 -> L4, 120 -> L5
            // ---------------------------------------------------------

            let mut indexes = [dummy, dummy, dummy];

            let index_ranges = [
                (0usize, 20, 0usize, 40, 1usize),
                (2usize, 60, 2usize, 80, 3usize),
                (4usize, 100, 4usize, 120, 5usize),
            ];

            for (i, &(data0, _, leaf0, data1, leaf1)) in index_ranges.iter().enumerate() {
                let mut children = IndexIdOrdArr::new();

                assert!(
                    children
                        .try_insert_pair_by((data[data0], leaves[leaf0]), &idx_cmp,)
                        .is_succ()
                );

                assert!(
                    children
                        .try_insert_pair_by((data[data0 + 1], leaves[leaf0]), &idx_cmp,)
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

                let mut children = IndexIdOrdArr::new();

                assert!(
                    children
                        .try_insert_pair_by((data[data0 + 1], leaves[leaf0]), &idx_cmp,)
                        .is_succ()
                );

                // assert!(
                //     children
                //         .try_insert_pair_by(
                //             (data1, leaves[leaf1]),
                //             &idx_cmp,
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

            let mut root_children = IndexIdOrdArr::new();

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

    #[test]
    fn fixture_leaf_chain_is_correct() {
        let fx = SearchFixture::new();

        // 验证每个叶子节点的 prev/next 指针
        for i in 0..6 {
            let node = fx.leaves[i].get(&fx.node_arena);
            if let TreeNode::Leaf(leaf) = node {
                let prev_expected = if i == 0 { fx.leaves[i] } else { fx.leaves[i - 1] };
                let next_expected = if i == 5 { fx.leaves[i] } else { fx.leaves[i + 1] };
                assert_eq!(leaf.prev_sibl_, prev_expected);
                assert_eq!(leaf.next_sibl_, next_expected);

                // 验证叶子内的 key 顺序
                let mut prev_key = None;
                for data_id in leaf.order_arr_.iter() {
                    let (k, _v) = data_id.get(&fx.data_arena);
                    if let Some(pk) = prev_key {
                        assert!(pk < k, "keys should be strictly increasing in leaf");
                    }
                    prev_key = Some(*k);
                }
            } else {
                panic!("expected leaf node");
            }
        }
    }

    #[test]
    fn fixture_root_and_indexes_correct() {
        let fx = SearchFixture::new();

        // 验证 root sentinel
        let root_node = fx.root.get(&fx.node_arena);
        if let TreeNode::Root(r) = root_node {
            let sentinel = r.sentinel_.as_ref().expect("root sentinel set");
            assert_eq!(sentinel.head_, fx.leaves[0]);
            assert_eq!(sentinel.tail_, fx.leaves[5]);

            // root children 对应于 indexes
            let mut idxs = Vec::new();
            for (idx, _) in r.children_.iter() {
                idxs.push(*idx);
            }
            assert_eq!(idxs.len(), fx.indexes.len());
            for &i in &fx.indexes {
                assert!(idxs.contains(&i));
            }
        } else {
            panic!("expected root node");
        }

        // 验证每个 index 的 parent 已指向 root，并且 children_ 非空
        for &index in &fx.indexes {
            let node = index.get(&fx.node_arena);
            if let TreeNode::Index(inode) = node {
                assert_eq!(inode.parent_, fx.root);
                assert!(!inode.children_.is_empty());
            } else {
                panic!("expected index node");
            }
        }
    }

    // TODO: 当 search 相关函数实现后，取消 ignore 并完成断言
    #[test]
    #[ignore]
    fn partition_point_recursive_placeholder() {
        let fx = SearchFixture::new();

        // 示例：寻找第一个 key >= 35
        use core::cmp::Ordering;

        let hint = ();

        let res = partition_point_recursive_(&fx.root, &fx.node_arena, &hint, |k: &KeyI32, _h: &()| {
            // pred 返回 true 当 key < 35
            *k < 35
        });

        // 期望返回 Data 或 Node 的位置，视实现而定
        // 这里仅打印以便手动检查。
        match res {
            PartitionPointIdx::Data(d) => {
                let (k, v) = d.get(&fx.data_arena);
                eprintln!("found data: {} -> {}", k, v);
            }
            PartitionPointIdx::Node(n) => {
                eprintln!("found node id: {:?}", n);
            }
        }
    }
}
