// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_VM_ADDRESS_REGION_SUBTREE_STATE_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_VM_ADDRESS_REGION_SUBTREE_STATE_H_

#include <assert.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <stddef.h>
#include <sys/types.h>

#include <fbl/intrusive_wavl_tree.h>
#include <ktl/algorithm.h>
#include <ktl/declval.h>
#include <ktl/type_traits.h>
#include <lockdep/guard.h>

//
// # VmAddressRegion Augmented Binary Search Tree Support
//
// The following types provide the state and tree maintenance hooks to implement an augmented binary
// search tree for VmAddressRegions. The augmentation maintains information about the extents and
// maximal gap between nodes for every subtree in the ordered set of allocated subregions, allowing
// efficient queries for unallocated gaps that satisfy size, alignment, compactness, and randomness
// requirements.
//
// ## General Approach
//
// VmAddressRegion maintains an ordered set of non-overlapping subregions, sorted by base address.
// The subregions, characterized by base address and size, are instances of either VmMapping or
// VmAddressRegion, supporting access to memory objects and recursive address space subdivision,
// respectively. The set of subregions is stored in a RegionList instance, which uses fbl::WAVLTree
// to implement the ordered set. The approach described here takes advantage of the self-balancing
// binary tree implementation of fbl::WAVLTree improve the time complexity of region allocation over
// naive sequential probing.
//
// ### Base Representation
//
// The following diagram is a linear representation of an address region covering addresses 0 to 20
// with four allocated subregions (small numbers are used for simplicity). The boxes represent
// allocated non-overlapping subregions with extents labeled <first byte>,<last byte>. The actual
// implementation stores base address and size, however, this representation is isomorphic and
// convenient for avoiding overflow in the calculations discussed later.
//
//   +-------+       +-------+---------------+                   +-----------------------+
//   |  0,1  |       |  4,5  |      6,9      |                   |         15,20         |
//   +-------+       +-------+---------------+                   +-----------------------+
//     0   1   2   3   4   5   6   7   8   9   10  11  12  13  14  15  16  17  18  19  20
//
// The following diagram illustrates the same allocated regions in a balanced binary tree
// representation, sorted by <first byte>. The node covering the range 4,5 is the root of the tree,
// and has the nodes covering 0,1 and 15,20 as left and right children, respectively. The node
// covering 15,20 has the node covering 6,9 as the left child and no right child. This structure is
// similar to what we would see in a WAVLTree, but can vary depending on the order nodes are added
// to the tree.
//
//                               +-------+
//                 --------------|  4,5  |--------------+
//                 |             +-------+              |
//                 V                                    V
//             +-------+                            +-------+
//             |  0,1  |                     +------| 15,20 |
//             +-------+                     |      +-------+
//                                           V
//                                       +-------+
//                                       |  6,9  |
//                                       +-------+
//
// This structure supports efficient upper/lower bound and address intersection queries in O(log n)
// time complexity, where n is the number of allocated regions in the tree. However, finding a gap
// with suitable properties requires O(m log n) time, where m is the number of adjacent node
// traversals, since finding the next adjacent node takes O(log n) time and finding a suitable gap
// takes m traversals of adjacent nodes using sequential probing.
//
// ### Augmented Representation
//
// The augmented representation builds on the base by storing and maintaining the extents and
// maximal gap size of the subtree of each node, in addition to the extents of the node itself. This
// information allows more efficient traversal by skipping over entire subtrees that have
// insufficient gaps for the requested allocation size.
//
// The following diagram illustrates the augmented balanced binary tree representation for the same
// allocated regions as the previous illustration.
//
//                        2      +-------+      0
//                 --------------|  4,5  |--------------+
//                 |             |  0,20 |              |
//                 |             |   5   |              |
//                 V             +-------+              V
//             +-------+                        5   +-------+
//             |  0,1  |                     +------| 15,20 |
//             |  0,1  |                     |      |  6,20 |
//             |   0   |                     |      |   5   |
//             +-------+                     V      +-------+
//                                       +-------+
//                                       |  6,9  |
//                                       |  6,9  |
//                                       |   0   |
//                                       +-------+
//
// Each node is labeled with the following three rows:
// 1. The base extents <first byte>,<last byte>.
// 2. The inclusive extents of the node's subtree <min first byte>,<max last byte>.
// 3. The inclusive <max gap> between any adjacent extents in a node's subtree.
//
// A node's <first byte> is adjacent to the <max last byte> of its left child and a node's <last
// byte> is adjacent to the <min first byte> of its right child.
//
// Each edge from parent to child in the diagram is labeled with the gap size between the node's
// extents and the adjacent subtree extents. The sizes of the left and right gaps are not stored
// directly in the tree, however, they may be computed in order constant time at any node using only
// the node and its immediate children.
//
//   left_gap = node.left ? node.first_byte - node.left.max_last_byte - 1 : 0
//   right_gap = node.right ? node.right.min_first_byte - node.last_byte - 1 : 0
//
// The maximal gap size of the subtree at any node is the maximum of its own left and right gaps and
// the maximal gaps of the left and right child subtrees.
//
//   max_left_gap = node.left ? node.left.max_gap : 0
//   max_right_gap = node.right ? node.right.max_gap : 0
//   node.max_gap = max(left_gap, right_gap, max_left_gap, max_right_gap)
//
// The minimum and maximum extents of a node's subtree are simply the minimum extent of the left
// subtree and maxium extent of the right subtree, respectively.
//
//   node.min_first_byte = node.left ? node.left.min_first_byte : node.first_byte
//   node.max_last_byte = node.right ? node.right.max_last_byte : node.last_byte
//
// Changes to the structure of the tree, described in the next section, may invalidate a node's
// subtree extents and maximal gap size. The aforementioned relationships between a node and it's
// direct children permit local restoration of the invalidated values in order constant time.
//
// ### Maintaining Augmented Invariants
//
// Insertions, deletions, and rotations are tree mutations that invalidate the augmented invariants
// of related nodes in the tree. The augmented invariants can be restored at key points during tree
// mutations to maintain the overall invariants of the tree needed for efficient traversal. These
// key restoration points are handled by the subset of the WAVLTree Observer interface implemented
// below.
//
// The following subsections describe each mutation and the associated restoration operation.
//
// #### Insertion
//
// Insertion involves descending the tree to find the correct sort order location for the new node,
// which becomes a new leaf having no children. The augmented invariants of a leaf node are a
// maximum subtree gap of zero and min/max subtree extents equal to the node extents.
//
// Before insertion, the augmented state of a new node is initialized as follows:
// - <min first byte> = <first byte>
// - <max last byte> = <last byte>
// - <max gap> = 0
//
// Upon insertion, the augmented invariants of the nodes along the path from the insertion point to
// the root are invalidated. Restoring the invariants is accomplished by re-computing the augmented
// values of each ancestor sequentially back to the root.
//
// #### Deletion
//
// Deleting a node results in similar invalidation of ancestor invariants to insertion. The same
// process to restore invariants after insertion is used for deletion. Deleting a non-leaf node may
// involve more steps, depending on the number of children it has. However, the invalidated ancestor
// path and restoration process is the same.
//
// #### Rotation
//
// Rotations are order constant operations that change the connections of closely related nodes in a
// binary tree without affecting the overall order invariant of the tree. Rotations are used to
// rebalance a binary tree after insertions or deletions. Like other mutations, rotations invalidate
// augmented invariants and require additional operations to restore invariants. However, a single
// rotation only affects the augmented invariants of two nodes, called the parent and pivot, rather
// than the entire ancestor path.
//
// In a rotation, the parent and pivot nodes swap positions as the root of the subtree and the
// parent adopts one of the pivot's children, depending on the direction of rotation. While the
// augmented invariants of both pivot and parent are invalidated, only the parent node needs to be
// restored from the state of its children, since one child changes. The augmented invariants of the
// overall subtree have not changed: the pivot can simply assume the augmented values of the
// original parent.
//

// Stores the subtree data values for the augmented binary search tree.
class VmAddressRegionSubtreeState {
 public:
  VmAddressRegionSubtreeState() = default;

  // Forward declaration of the fbl::WAVLTree observer that provides hooks to maintain these values
  // during tree mutations.
  template <typename>
  class Observer;

  // Returns the minimum address of this node's subtree.
  vaddr_t min_first_byte() const { return min_first_byte_; }

  // Returns the maximum address of this node's subtree.
  vaddr_t max_last_byte() const { return max_last_byte_; }

  // Returns the maximum gap between any adjacent nodes in this node's subtree.
  size_t max_gap() const { return max_gap_; }

 private:
  VmAddressRegionSubtreeState(const VmAddressRegionSubtreeState&) = default;
  VmAddressRegionSubtreeState& operator=(const VmAddressRegionSubtreeState&) = default;

  vaddr_t min_first_byte_;
  vaddr_t max_last_byte_;
  size_t max_gap_;
};

// fbl::WAVLTree observer providing hooks to maintain the augmented invariants for tree nodes during
// mutations.
//
// The Node parameter is the underlying node type stored in the fbl::WAVLTree.
//
// Example usage:
//
//  using KeyType = vaddr_t;
//  using NodeType = VmNodeType;
//  using PtrType = fbl::RefPtr<NodeType>;
//  using KeyTraits = fbl::DefaultKeyedObjectTraits<...>;
//  using TagType = fbl::DefaultObjectTag;
//  using NodeTraits = fbl::DefaultWAVLTreeTraits<PtrType, TagType>;
//  using Observer = VmAddresRegionSubtreeState::Observer<NodeType>;
//  using TreeType = fbl::WAVLTree<KeyType, PtrType, KeyTraits, TagType, NodeTraits, Observer>;
//
// See fbl::tests::intrusive_containers::DefaultWAVLTreeObserver for a detailed explanation of the
// semantics of these hooks in the context of fundamental fbl::WAVLTree operations.
template <typename Node>
class VmAddressRegionSubtreeState::Observer {
  // Check that the Node type has the minimum required methods.
  template <typename T, typename = void>
  struct CheckMethods : ktl::false_type {};
  template <typename T>
  struct CheckMethods<T, ktl::void_t<decltype(ktl::declval<Node>().lock_ref()),
                                     decltype(ktl::declval<Node>().base_locked()),
                                     decltype(ktl::declval<Node>().size_locked()),
                                     decltype(ktl::declval<Node>().subtree_state_locked())>>
      : ktl::true_type {};
  static_assert(CheckMethods<Node>::value, "Node type does not implement the required interface.");

  // Evaluates to the const or non-const state type returned by Iter::subtree_state_locked().
  template <typename Iter>
  using StateType = ktl::remove_reference_t<decltype(ktl::declval<Iter>()->subtree_state_locked())>;

  // Enable if the state type is const, implying that the iterator is const.
  template <typename Iter>
  using RequiresConstIter = ktl::enable_if_t<ktl::is_const_v<StateType<Iter>>, int>;

  // Enable if the state type is non-const, implying that the iterator is non-const.
  template <typename Iter>
  using RequiresNonConstIter = ktl::enable_if_t<!ktl::is_const_v<StateType<Iter>>, int>;

 public:
  // Restores invalidated invariants from the given node to the root.
  template <typename Iter, RequiresNonConstIter<Iter> = 0>
  static void RestoreInvariants(Iter node) {
    AssertHeld(node->lock_ref());
    PropagateToRoot(node);
  }

  // Immutable accessors. These accessors bypass lock analysis for simplicity, since they are called
  // internally by the fbl::WAVLTree instance and the RegionList, which are collectively protected
  // by the same lock.
  template <typename Iter>
  static vaddr_t FirstByte(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->base_locked();
  }
  template <typename Iter>
  static vaddr_t LastByte(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->base_locked() + node->size_locked() - 1;
  }
  template <typename Iter, RequiresConstIter<Iter> = 0>
  static vaddr_t MinFirstByte(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->subtree_state_locked().min_first_byte_;
  }
  template <typename Iter, RequiresConstIter<Iter> = 0>
  static vaddr_t MaxLastByte(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->subtree_state_locked().max_last_byte_;
  }
  template <typename Iter, RequiresConstIter<Iter> = 0>
  static size_t MaxGap(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->subtree_state_locked().max_gap_;
  }

  // Computes the gap size between adjacent extents.
  static size_t Gap(vaddr_t left_last_byte, vaddr_t right_first_byte) {
    DEBUG_ASSERT(left_last_byte < right_first_byte);
    return right_first_byte - left_last_byte - 1;
  }

 private:
  template <typename, typename, typename, typename, typename, typename>
  friend class fbl::WAVLTree;

  // Mutable accessors. These accessors bypass lock analysis for simplicity, since they are called
  // internally by the fbl::WAVLTree instance and the RegionList, which are collectively protected
  // by the same lock.
  template <typename Iter>
  static VmAddressRegionSubtreeState& State(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->subtree_state_locked();
  }
  template <typename Iter, RequiresNonConstIter<Iter> = 0>
  static vaddr_t& MinFirstByte(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->subtree_state_locked().min_first_byte_;
  }
  template <typename Iter, RequiresNonConstIter<Iter> = 0>
  static vaddr_t& MaxLastByte(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->subtree_state_locked().max_last_byte_;
  }
  template <typename Iter, RequiresNonConstIter<Iter> = 0>
  static size_t& MaxGap(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->subtree_state_locked().max_gap_;
  }

  // Sets the initial state of the node for insertion as a leaf.
  template <typename Iter>
  static void Initialize(Iter node) {
    MinFirstByte(node) = FirstByte(node);
    MaxLastByte(node) = LastByte(node);
    MaxGap(node) = 0;
  }

  // Updates the state of a node from its immediate children, restoring any invalid invariants. The
  // left and right children are passed in explicitly, instead of using the left() and right()
  // iterator methods, to accommodate the rotation hook. RecordRotation() is called before the
  // pointers are updated during a rotation operation -- either the left() or right() accessor at
  // the time the hook is called does not reflect the post-rotation child of the node.
  template <typename Iter>
  static void Update(Iter node, Iter left, Iter right) {
    size_t left_max_gap = 0;
    size_t right_max_gap = 0;
    if (left) {
      const size_t left_gap = Gap(MaxLastByte(left), FirstByte(node));
      left_max_gap = ktl::max(left_gap, MaxGap(left));
      MinFirstByte(node) = MinFirstByte(left);
    } else {
      MinFirstByte(node) = FirstByte(node);
    }
    if (right) {
      const size_t right_gap = Gap(LastByte(node), MinFirstByte(right));
      right_max_gap = ktl::max(right_gap, MaxGap(right));
      MaxLastByte(node) = MaxLastByte(right);
    } else {
      MaxLastByte(node) = LastByte(node);
    }
    MaxGap(node) = ktl::max(left_max_gap, right_max_gap);
  }

  // Updates the nodes in the path from the given node to the root.
  template <typename Iter>
  static void PropagateToRoot(Iter node) {
    while (node) {
      Update(node, node.left(), node.right());
      node = node.parent();
    }
  }

  // Initializes the inserted node and restores invalidated invariants along the path to the root.
  template <typename Iter>
  static void RecordInsert(Iter node) {
    Initialize(node);
    PropagateToRoot(node.parent());
  }

  // Restores invariants invalidated by a left or right rotation, with the pivot inheriting the
  // overall state of the subtree and the original parent updated to reflect its new child. This
  // hook is called before updating the pointers the respective nodes. The direction of the rotation
  // is determined by whether the pivot is the left or right child of the parent.
  //
  // The following diagrams the relationship of the nodes in a left rotation:
  //
  //             pivot                          parent                             |
  //            /     \                         /    \                             |
  //        parent  rl_child  <-----------  sibling  pivot                         |
  //        /    \                                   /   \                         |
  //   sibling  lr_child                       lr_child  rl_child                  |
  //
  // In a right rotation, all of the relationships are reflected, such that the arguments to this
  // hook are in the same order in both rotation directions. In particular, the parent node always
  // inherits the lr_child node. Notionally, "lr_child" means left child in left rotation or right
  // child in right rotation and "rl_child" means right child in left rotation or left child in
  // right rotation.
  template <typename Iter>
  static void RecordRotation(Iter pivot, Iter lr_child, Iter rl_child, Iter parent, Iter sibling) {
    DEBUG_ASSERT((parent.right() == pivot) ^ (parent.left() == pivot));
    State(pivot) = State(parent);
    if (parent.right() == pivot) {
      Update(parent, sibling, lr_child);
    } else {
      Update(parent, lr_child, sibling);
    }
  }

  // Restores invariants along the path from the invalidation (removal) point to the root.
  template <typename Iter>
  static void RecordErase(Node* node, Iter invalidated) {
    PropagateToRoot(invalidated);
  }

  // Collision and replacement operations are not supported in this application of the WAVLTree.
  // Assert that these operations do not occur.
  template <typename Iter>
  static void RecordInsertCollision(Node* node, Iter collision) {
    DEBUG_ASSERT(false);
  }
  template <typename Iter>
  static void RecordInsertReplace(Iter node, Node* replacement) {
    DEBUG_ASSERT(false);
  }

  // These hooks are not needed by this application of the WAVLTree.
  template <typename Iter>
  static void RecordInsertTraverse(Node* node, Iter ancestor) {}
  static void RecordInsertPromote() {}
  static void RecordInsertRotation() {}
  static void RecordInsertDoubleRotation() {}
  static void RecordEraseDemote() {}
  static void RecordEraseRotation() {}
  static void RecordEraseDoubleRotation() {}
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_VM_ADDRESS_REGION_SUBTREE_STATE_H_
