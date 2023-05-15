// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_ATTRIBUTION_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_ATTRIBUTION_H_

#include <lib/relaxed_atomic.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>

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

class AttributionObjectNode : public fbl::DoublyLinkedListable<AttributionObjectNode*> {
 public:
  // Protects the global list of the attribution objects.
  DECLARE_SINGLETON_CRITICAL_MUTEX(AllAttributionObjectsLock);

  // Inserts a new node in the list before the given existing node (or at the
  // end if |where| is nullptr).
  static void AddToGlobalListLocked(AttributionObjectNode* where, AttributionObjectNode* node)
      TA_REQ(AllAttributionObjectsLock::Get());

  // Removes a node from the list.
  static void RemoveFromGlobalListLocked(AttributionObjectNode* node)
      TA_REQ(AllAttributionObjectsLock::Get());

 private:
  // All the nodes in the system.
  static fbl::DoublyLinkedList<AttributionObjectNode*> all_nodes_
      TA_GUARDED(AllAttributionObjectsLock::Get());
};

/// AttributionObject is a collection of counters that track page
/// allocations and deallocations of VmObjectPageds owned by the
/// process identified by `owning_koid_`.
class AttributionObject : public fbl::RefCounted<AttributionObject>, private AttributionObjectNode {
 public:
  ~AttributionObject();

  static void KernelAttributionInit();

  // Provides a statically defined AttributionObject that tracks
  // all page allocations/deallocations in VMOs owned by the
  // kernel. The returned value is provided to VmObjectPaged::create
  // calls for vmos created by the kernel, and is stored as a member
  // variable on the backing VmCowPages.
  static fbl::RefPtr<AttributionObject> GetKernelAttribution() {
    return fbl::RefPtr<AttributionObject>(&kernel_attribution_);
  }

  // Sets the owning koid and inserts this attribution object in the list before
  // the given existing node.
  void AddToGlobalListWithKoid(AttributionObjectNode* where, zx_koid_t owning_koid)
      TA_EXCL(AllAttributionObjectsLock::Get());

  // Increment allocation counters by `page_count`.
  // total_resident_pages_allocated_ is unconditionally incremented.
  // private_resident_pages_allocated_ is only incremented when
  // `is_shared` is false.
  void AddPages(size_t page_count, bool is_shared) {
    total_resident_pages_allocated_.fetch_add(page_count);

    if (!is_shared) {
      private_resident_pages_allocated_.fetch_add(page_count);
    }
  }

  // Increment deallocation counters by page_count.
  // total_resident_pages_deallocated_ is unconditionally incremented.
  // private_resident_pages_deallocated_ is only incremented when
  // `is_shared` is false.
  void RemovePages(size_t page_count, bool is_shared) {
    total_resident_pages_deallocated_.fetch_add(page_count);

    if (!is_shared) {
      private_resident_pages_deallocated_.fetch_add(page_count);
    }
  }

  zx_koid_t GetOwningKoid() const { return owning_koid_; }

 private:
  // Total number of pages made resident for VmObjectPageds
  // that are uniquely owned by owning_koid_.
  RelaxedAtomic<uint64_t> private_resident_pages_allocated_{0};
  // Total number of pages that have been released from
  // VmObjectPageds that are uniquely owned by owning_koid_.
  RelaxedAtomic<uint64_t> private_resident_pages_deallocated_{0};
  // Total number of pages made resident for any VmObjectPaged
  // being attributed to owning_koid_.
  RelaxedAtomic<uint64_t> total_resident_pages_allocated_{0};
  // Total number of pages that have been released from any
  // VmObjectPageds being attributed to owning_koid_.
  RelaxedAtomic<uint64_t> total_resident_pages_deallocated_{0};
  // The koid of the process that this attribution object is tracking.
  zx_koid_t owning_koid_;

  // The attribution object used to track resident memory
  // for VMOs attributed to the kernel.
  static AttributionObject kernel_attribution_;
};

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_ATTRIBUTION_H_
