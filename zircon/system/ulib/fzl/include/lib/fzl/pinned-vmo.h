// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_FZL_PINNED_VMO_H_
#define LIB_FZL_PINNED_VMO_H_

#include <zircon/types.h>

#include <utility>

#include <fbl/macros.h>
#include <fbl/ref_ptr.h>
#include <object/bus_transaction_initiator_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/pinned_vm_object.h>
#include <vm/vm_address_region.h>

namespace fzl {

class PinnedVmo {
 public:
  struct Region {
    zx_paddr_t phys_addr;
    uint64_t size;
  };

  PinnedVmo() = default;
  ~PinnedVmo() { Unpin(); }
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(PinnedVmo);

  // Move support
  PinnedVmo(PinnedVmo&& other) { *this = std::move(other); }

  PinnedVmo& operator=(PinnedVmo&& other) {
    pmt_ = std::move(other.pmt_);
    regions_ = std::move(other.regions_);
    region_count_ = other.region_count_;
    other.region_count_ = 0;
    return *this;
  }

  zx_status_t Pin(const fbl::RefPtr<VmObjectDispatcher>& vmo,
                  const fbl::RefPtr<BusTransactionInitiatorDispatcher>& bti, uint32_t options);
  zx_status_t PinRange(uint64_t offset, uint64_t len, const fbl::RefPtr<VmObjectDispatcher>& vmo,
                       const fbl::RefPtr<BusTransactionInitiatorDispatcher>&, uint32_t options);
  void Unpin();

  uint32_t region_count() const { return region_count_; }
  const Region& region(uint32_t ndx) const {
    ZX_DEBUG_ASSERT(ndx < region_count_);
    ZX_DEBUG_ASSERT(regions_ != nullptr);
    return regions_[ndx];
  }

 private:
  void UnpinInternal();
  zx_status_t PinInternal(uint64_t offset, uint64_t len, const fbl::RefPtr<VmObjectDispatcher>& vmo,
                          const fbl::RefPtr<BusTransactionInitiatorDispatcher>& bti,
                          uint32_t options);

  KernelHandle<PinnedMemoryTokenDispatcher> pmt_;
  ktl::unique_ptr<Region[]> regions_;
  uint32_t region_count_ = 0;
};

}  // namespace fzl

#endif  // LIB_FZL_PINNED_VMO_H_
