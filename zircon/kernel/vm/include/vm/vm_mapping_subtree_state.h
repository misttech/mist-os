// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_VM_MAPPING_SUBTREE_STATE_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_VM_MAPPING_SUBTREE_STATE_H_

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
// # VmMapping Augmented Binary Search Tree Support
//
// The following types provide the state and tree maintenance hooks to implement an augmented binary
// search tree for VmMappings. The augmentation maintains information about the largest mapping end
// address in the subregion, allowing for efficiently finding all mappings that overlap with a given
// range.
//
// ## General Approach
//
// VmObject maintains an ordered set of possibly overlapping mappings sorted by base offset, with
// the object heap address used as a secondary sorting key for mappings that share the base offset.
// The mappings, characterized by base offset and size, are instances of a VmMapping. Many different
// mappings might reference the same offset.
//
// The set of mappings is stored in a fbl::WAVLTree and the approach described here takes
// advantage of the self-balancing binary tree implementation to improve the time complexity of
// finding the set of mappings that might contain a range of offsets.
//
// ### Base Representation
//
// The following diagram is a linear representation of the offsets covering addresses 0 to 20
// with six mappings (small numbers are used for simplicity) across two lines due to overlap. The
// boxes represent mappings labeled <first offset>,<last offset>. The actual implementation stores
// base offset and size, however, this representation is isomorphic and convenient for avoiding
// overflow in the calculations discussed later.
//
//   +-------+       +---------------------------+                +-----------------------+
//   |  0,1  |       |           4,10            |                |         15,20         |
//   +-------+       +---------------------------+                +-----------------------+
//               +-----------+---------------+--------------------------------+
//               |    3,5    |      6,8      |               9,17             |
//               +-----------+---------------+--------------------------------+
//     0   1   2   3   4   5   6   7   8   9   10  11  12  13  14  15  16  17  18  19  20
//
// The following diagram illustrates the same mappings in a balanced binary tree
// representation, sorted by <first offset>. This structure is similar to what we would see in a
// WAVLTree, but can vary depending on the order nodes are added to the tree.
//
//                                 +-------+
//                  +--------------|  6,8  |--------------+
//                  |              +-------+              |
//                  V                                     V
//              +-------+                             +-------+
//       +------|  3,5  |------+               +------| 15,20 |
//       |      +-------+      |               |      +-------+
//       V                     V               V
//   +-------+             +-------+       +-------+
//   |  0,1  |             | 4,10  |       | 9,17  |
//   +-------+             +-------+       +-------+
//
// This structure supports efficient searches for mappings that begin at a particular offset in
// O(log n) time. However, finding all mappings that cover a particular offset requires, in the
// worst case, a full tree walk, since no information about the size of the mappings is encoded in
// the tree. Therefore as long as the base offset is below the search offset, any mapping somewhere
// in the subtree could extend into the search offset.
//
// ### Augmented Representation
//
// The augmented representation builds on the base by storing and maintaining the largest end
// offset of the of the subtree of each node. This allows for skipping subtrees that cannot have a
// mapping that might contain the search offset.
//
// The following diagram illustrates the augmented balanced binary tree representation for the same
// allocated regions as the previous illustration.
//
//                                 +-------+
//                  +--------------|  6,8  |--------------+
//                  |              |  20   |              |
//                  |              +-------+              |
//                  V                                     V
//              +-------+                             +-------+
//       +------|  3,5  |------+               +------| 15,20 |
//       |      |  10   |      |               |      |  20   |
//       |      +-------+      |               |      +-------+
//       V                     V               V
//   +-------+             +-------+       +-------+
//   |  0,1  |             | 4,10  |       | 9,17  |
//   |   1   |             |  10   |       |  17   |
//   +-------+             +-------+       +-------+
//
// The new row in each node is the maximum end offset for that nodes subtree. This maximum offset is
// always maintained as:
//
//   node.max_offset = max(left_node.max_offset, right_node.max_offset, node.last_offset)
//
// Changes to the structure of the tree, described in the next section, may invalidate a node's
// subtree extents and max offset. The aforementioned relationships between a node and it's
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
// which becomes a new leaf having no children, and its max_offset can be calculated as normal.
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
class VmMappingSubtreeState {
 public:
  VmMappingSubtreeState() = default;

  // Forward declaration of the fbl::WAVLTree observer that provides hooks to maintain these values
  // during tree mutations.
  template <typename>
  class Observer;

  // Returns the maximum last offsets of this node's subtree.
  uint64_t max_last_offset() const { return max_last_offset_; }

 private:
  VmMappingSubtreeState(const VmMappingSubtreeState&) = default;
  VmMappingSubtreeState& operator=(const VmMappingSubtreeState&) = default;

  uint64_t max_last_offset_ = 0;
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
//  using Observer = VmAddressRegionSubtreeState::Observer<NodeType>;
//  using TreeType = fbl::WAVLTree<KeyType, PtrType, KeyTraits, TagType, NodeTraits, Observer>;
//
// See fbl::tests::intrusive_containers::DefaultWAVLTreeObserver for a detailed explanation of the
// semantics of these hooks in the context of fundamental fbl::WAVLTree operations.
template <typename Node>
class VmMappingSubtreeState::Observer {
  // Check that the Node type has the minimum required methods.
  template <typename T, typename = void>
  struct CheckMethods : ktl::false_type {};
  template <typename T>
  struct CheckMethods<T, ktl::void_t<decltype(ktl::declval<Node>().lock_ref()),
                                     decltype(ktl::declval<Node>().object_offset_locked_object()),
                                     decltype(ktl::declval<Node>().size_locked_object())>>
      : ktl::true_type {};
  static_assert(CheckMethods<Node>::value, "Node type does not implement the required interface.");

 public:
  // Restores invalidated invariants from the given node to the root.
  template <typename Iter>
  static void RestoreInvariants(Iter node) {
    AssertHeld(node->lock_ref());
    PropagateToRoot(node);
  }

  // Immutable accessors. These accessors bypass lock analysis for simplicity, since they are called
  // internally by the fbl::WAVLTree instance and the mapping list, which are collectively protected
  // by the same lock.
  template <typename Iter>
  static uint64_t FirstOffset(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->object_offset_locked_object();
  }
  template <typename Iter>
  static uint64_t LastOffset(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->object_offset_locked_object() + (node->size_locked_object() - 1);
  }
  template <typename Iter>
  static uint64_t MaxLastOffset(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return State(node).max_last_offset_;
  }

 private:
  template <typename, typename, typename, typename, typename, typename>
  friend class fbl::WAVLTree;

  // Mutable accessor. This accessor bypasses lock analysis for simplicity, since it is called
  // internally by the fbl::WAVLTree instance and the mapping list, which are collectively protected
  // by the same lock.
  template <typename Iter>
  static VmMappingSubtreeState& State(Iter node) TA_NO_THREAD_SAFETY_ANALYSIS {
    return node->mapping_subtree_state_;
  }

  // Updates the state of a node from its immediate children, restoring any invalid invariants. The
  // left and right children are passed in explicitly, instead of using the left() and right()
  // iterator methods, to accommodate the rotation hook. RecordRotation() is called before the
  // pointers are updated during a rotation operation -- either the left() or right() accessor at
  // the time the hook is called does not reflect the post-rotation child of the node.
  template <typename Iter>
  static void Update(Iter node, Iter left, Iter right) {
    uint64_t max_last_offset = LastOffset(node);
    if (left) {
      max_last_offset = ktl::max(max_last_offset, MaxLastOffset(left));
    }
    if (right) {
      max_last_offset = ktl::max(max_last_offset, MaxLastOffset(right));
    }
    State(node).max_last_offset_ = max_last_offset;
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
    State(node).max_last_offset_ = LastOffset(node);
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
  template <typename T, typename Iter>
  static void RecordErase(T* node, Iter invalidated) {
    PropagateToRoot(invalidated);
  }

  // Collision and replacement operations are not supported in this application of the WAVLTree.
  // Assert that these operations do not occur.
  template <typename T, typename Iter>
  static void RecordInsertCollision(T* node, Iter collision) {
    DEBUG_ASSERT(false);
  }
  template <typename Iter, typename T>
  static void RecordInsertReplace(Iter node, T* replacement) {
    DEBUG_ASSERT(false);
  }

  // These hooks are not needed by this application of the WAVLTree.
  template <typename T, typename Iter>
  static void RecordInsertTraverse(T* node, Iter ancestor) {}
  static void RecordInsertPromote() {}
  static void RecordInsertRotation() {}
  static void RecordInsertDoubleRotation() {}
  static void RecordEraseDemote() {}
  static void RecordEraseRotation() {}
  static void RecordEraseDoubleRotation() {}
};

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_VM_MAPPING_SUBTREE_STATE_H_
