// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_ATTRIBUTION_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_ATTRIBUTION_H_

#include <lib/kconcurrent/seqlock.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>

// There is one attribution object per process, and an extra one for the
// kernel's own memory (AttributionObject::kernel_attribution_). Each
// ProcessDispatcher contains a pointer to the corresponding AttributionObject.
//
// In addition to those references, we also keep a linked lists of all the
// attribution objects, which is used to answer queries more quickly. In detail,
// two types of queries are possible:
//
// - Queries directed to a specific process: those are answered by simply
//   following the ProcessDispatcher's pointer to the AttributionObject.
//
// - Queries directed to a specific job, which return the list of all the
//   attribution objects belonging to descendant processes. In order to quickly
//   locate the entries belonging to a given job, we add two sentinel nodes in
//   the linked list for each job, with the property that all the nodes
//   corresponding to descendant jobs/processes must be stored between them.
//   Retrieving all the attribution objects under a given job becomes then a
//   matter of simply scanning the list, starting from the job's first sentinel
//   node and stopping at the second one.
//
// Nodes are inserted in this list when the corresponding ProcessDispatcher or
// JobDispatcher is created and removed when the corresponding dispatcher is
// destroyed, with an exception: attribution object nodes may outlive the
// corresponding ProcessDispatcher if, after the process exits, a non-zero
// number of VmCowPages are still attributed to that process. This can happen
// if VMOs that are kept alive by references in other processes were attributed
// to the exited process. This exception is not written explicitly: it results
// from our usage of RefPtrs. Such attribution objects are only reachable by
// scanning the list, e.g. by querying one of their ancestor jobs.

class AttributionObject;

class AttributionObjectNode : public fbl::DoublyLinkedListable<AttributionObjectNode*> {
 public:
  // Constructs a sentinel node by default.
  AttributionObjectNode() : AttributionObjectNode(NodeType::Sentinel) {}

  // Protects both the global list of the attribution objects and all its
  // cursors.
  DECLARE_SINGLETON_CRITICAL_MUTEX(AllAttributionObjectsLock);

  // Inserts a new node in the list before the given existing node (or at the
  // end if |where| is nullptr).
  static void AddToGlobalListLocked(AttributionObjectNode* where, AttributionObjectNode* node)
      TA_REQ(AllAttributionObjectsLock::Get());

  // Removes a node from the list and advances all the cursors pointing to it.
  static void RemoveFromGlobalListLocked(AttributionObjectNode* node)
      TA_REQ(AllAttributionObjectsLock::Get());

  // Attempts to downcast this node into an AttributionObject.
  AttributionObject* DowncastToAttributionObject();

 protected:
  enum class NodeType : bool { Sentinel = false, AttributionObject = true };
  explicit AttributionObjectNode(NodeType node_type) : node_type_(node_type) {}

 private:
  friend class AttributionObjectsCursor;

  const NodeType node_type_;

  // All the nodes in the system.
  static fbl::DoublyLinkedList<AttributionObjectNode*> all_nodes_
      TA_GUARDED(AllAttributionObjectsLock::Get());
};

/// AttributionObject is a collection of counters that track page
/// allocations and deallocations of VmObjectPageds owned by the
/// process identified by `owning_koid_`.
class AttributionObject : public fbl::RefCounted<AttributionObject>, public AttributionObjectNode {
 public:
  AttributionObject() : AttributionObjectNode(AttributionObjectNode::NodeType::AttributionObject) {}
  ~AttributionObject();

  // A placeholder pointer to AttributionObject that is always NULL.
  //
  // Used when KBMA is disabled by functions that return a const fbl::RefPtr<AttributionObject>&
  // as the return value.
#if !KERNEL_BASED_MEMORY_ATTRIBUTION
  static inline const fbl::RefPtr<AttributionObject> null_attribution_ptr_{nullptr};
#endif

  static void KernelAttributionInit();

  // Provides a statically defined AttributionObject that tracks
  // all page allocations/deallocations in VMOs owned by the
  // kernel. The returned value is provided to VmObjectPaged::create
  // calls for vmos created by the kernel, and is stored as a member
  // variable on the backing VmCowPages.
  static const fbl::RefPtr<AttributionObject>& GetKernelAttribution() {
#if KERNEL_BASED_MEMORY_ATTRIBUTION
    return kernel_attribution_object_;
#else
    return null_attribution_ptr_;
#endif
  }

  // Sets the owning koid and inserts this attribution object in the list before
  // the given existing node.
  void AddToGlobalListWithKoid(AttributionObjectNode* where, zx_koid_t owning_koid)
      TA_EXCL(AllAttributionObjectsLock::Get());

 private:
  // The koid of the process that this attribution object is tracking.
  zx_koid_t owning_koid_;

#if KERNEL_BASED_MEMORY_ATTRIBUTION
  // The attribution object used to track resident memory
  // for VMOs attributed to the kernel.
  static fbl::RefPtr<AttributionObject> kernel_attribution_object_;
#endif
};

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_ATTRIBUTION_H_
