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

/// AttributionObject is a collection of counters that track page
/// allocations and deallocations of VmObjectPageds owned by the
/// process identified by `owning_koid_`.
class AttributionObject : public fbl::RefCounted<AttributionObject>,
                          public fbl::DoublyLinkedListable<AttributionObject*> {
 public:
  AttributionObject() : owning_koid_(ZX_KOID_INVALID) {}
  ~AttributionObject();

  // No copy, move, or assignment.
  AttributionObject(const AttributionObject&) = delete;
  AttributionObject(AttributionObject&&) = delete;
  AttributionObject& operator=(const AttributionObject&) = delete;
  AttributionObject& operator=(AttributionObject&&) = delete;

  static void AddAttributionToGlobalList(AttributionObject* obj)
      TA_EXCL(AllAttributionObjectsLock::Get());
  static void RemoveAttributionFromGlobalList(AttributionObject* obj)
      TA_EXCL(AllAttributionObjectsLock::Get());
  static void KernelAttributionInit();

  // Provides a statically defined AttributionObject that tracks
  // all page allocations/deallocations in VMOs owned by the
  // kernel. The returned value is provided to VmObjectPaged::create
  // calls for vmos created by the kernel, and is stored as a member
  // variable on the backing VmCowPages.
  static fbl::RefPtr<AttributionObject> GetKernelAttribution() {
    return fbl::RefPtr<AttributionObject>(&kernel_attribution_);
  }

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

  void SetOwningKoid(zx_koid_t owning_koid) { owning_koid_ = owning_koid; }
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

  DECLARE_SINGLETON_MUTEX(AllAttributionObjectsLock);
  static fbl::DoublyLinkedList<AttributionObject*> all_attribution_objects_
      TA_GUARDED(AllAttributionObjectsLock::Get());

  // The attribution object used to track resident memory
  // for VMOs attributed to the kernel.
  static AttributionObject kernel_attribution_;
};

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_ATTRIBUTION_H_
