// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "object/pinned_memory_token_dispatcher.h"

#include <align.h>
#include <assert.h>
#include <lib/counters.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <new>

#include <ktl/algorithm.h>
#include <ktl/bit.h>
#include <object/bus_transaction_initiator_dispatcher.h>
#include <vm/pinned_vm_object.h>
#include <vm/vm.h>
#include <vm/vm_object.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

KCOUNTER(dispatcher_pinned_memory_token_create_count, "dispatcher.pinned_memory_token.create")
KCOUNTER(dispatcher_pinned_memory_token_destroy_count, "dispatcher.pinned_memory_token.destroy")

zx_status_t PinnedMemoryTokenDispatcher::Create(fbl::RefPtr<BusTransactionInitiatorDispatcher> bti,
                                                PinnedVmObject pinned_vmo, uint32_t perms,
                                                KernelHandle<PinnedMemoryTokenDispatcher>* handle,
                                                zx_rights_t* rights) {
  LTRACE_ENTRY;
  DEBUG_ASSERT(IS_PAGE_ALIGNED(pinned_vmo.offset()) && IS_PAGE_ALIGNED(pinned_vmo.size()));

  fbl::AllocChecker ac;
  KernelHandle new_handle(
      fbl::AdoptRef(new (&ac) PinnedMemoryTokenDispatcher(ktl::move(bti), ktl::move(pinned_vmo))));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = new_handle.dispatcher()->MapIntoIommu(perms);
  if (status != ZX_OK) {
    LTRACEF("MapIntoIommu failed: %d\n", status);
    return status;
  }

  // Create must be called with the BTI's lock held, so this is safe to
  // invoke.
  const fbl::RefPtr<PinnedMemoryTokenDispatcher>& dispatcher = new_handle.dispatcher();
  AssertHeld(*dispatcher->bti_->get_lock());
  dispatcher->bti_->AddPmoLocked(new_handle.dispatcher().get());
  dispatcher->initialized_ = true;

  *handle = ktl::move(new_handle);
  *rights = default_rights();
  return ZX_OK;
}

// Used during initialization to set up the IOMMU state for this PMT.
//
// We disable thread-safety analysis here, because this is part of the
// initialization routine before other threads have access to this dispatcher.
zx_status_t PinnedMemoryTokenDispatcher::MapIntoIommu(uint32_t perms) TA_NO_THREAD_SAFETY_ANALYSIS {
  DEBUG_ASSERT(!initialized_);

  const uint64_t bti_id = bti_->bti_id();
  if (pinned_vmo_.vmo()->is_contiguous()) {
    // Usermode drivers assume that if they requested a contiguous buffer in
    // memory, then the physical addresses will be contiguous.  Return an
    // error if we can't actually map the address contiguously.
    auto result = bti_->iommu().MapContiguous(bti_id, pinned_vmo_.vmo(), pinned_vmo_.offset(),
                                              pinned_vmo_.size(), perms);
    if (result.is_error() != ZX_OK) {
      return result.status_value();
    }
    map_token_ = *result;
    return ZX_OK;
  }

  auto result =
      bti_->iommu().Map(bti_id, pinned_vmo_.vmo(), pinned_vmo_.offset(), pinned_vmo_.size(), perms);
  if (result.is_error()) {
    return result.status_value();
  }
  map_token_ = *result;
  return ZX_OK;
}

zx_status_t PinnedMemoryTokenDispatcher::UnmapFromIommuLocked() {
  auto& iommu = bti_->iommu();
  const uint64_t bus_txn_id = bti_->bti_id();

  if (map_token_ == UINT64_MAX) {
    // No work to do, nothing is mapped.
    return ZX_OK;
  }
  zx_status_t status = iommu.Unmap(bus_txn_id, map_token_, pinned_vmo_.size());
  map_token_ = UINT64_MAX;
  return status;
}

void PinnedMemoryTokenDispatcher::Unpin() {
  Guard<CriticalMutex> guard{get_lock()};
  explicitly_unpinned_ = true;

  // Unmap the memory prior to unpinning to prevent continued access.
  zx_status_t status = UnmapFromIommuLocked();
  ASSERT(status == ZX_OK);

  // Move the pinned vmo to a temporary and to let its dtor unpin.
  auto destroy = ktl::move(pinned_vmo_);
}

void PinnedMemoryTokenDispatcher::on_zero_handles() {
  Guard<CriticalMutex> guard{get_lock()};

  if (!explicitly_unpinned_ && initialized_) {
    // The user failed to call zx_pmt_unpin. Unmap the memory to prevent continued access, but leave
    // the VMO pinned and use the quarantine mechanism to protect against stray DMA.
    zx_status_t status = UnmapFromIommuLocked();
    ASSERT(status == ZX_OK);

    bti_->Quarantine(fbl::RefPtr(this));
  }
}

PinnedMemoryTokenDispatcher::~PinnedMemoryTokenDispatcher() {
  kcounter_add(dispatcher_pinned_memory_token_destroy_count, 1);

  if (initialized_) {
    bti_->RemovePmo(this);
  }
}

PinnedMemoryTokenDispatcher::PinnedMemoryTokenDispatcher(
    fbl::RefPtr<BusTransactionInitiatorDispatcher> bti, PinnedVmObject pinned_vmo)
    : pinned_vmo_(ktl::move(pinned_vmo)), bti_(ktl::move(bti)) {
  DEBUG_ASSERT(pinned_vmo_.vmo() != nullptr);
  kcounter_add(dispatcher_pinned_memory_token_create_count, 1);
}

zx_status_t PinnedMemoryTokenDispatcher::EncodeAddrs(bool compress_results, bool contiguous,
                                                     dev_vaddr_t* mapped_addrs,
                                                     size_t mapped_addrs_count) {
  Guard<CriticalMutex> guard{get_lock()};

  const uint64_t bti_id = bti_->bti_id();

  if (compress_results) {
    const size_t min_contig = bti_->minimum_contiguity();
    DEBUG_ASSERT(ktl::has_single_bit(min_contig));
    const uint64_t num_addrs = ROUNDUP(pinned_vmo_.size(), min_contig) / min_contig;
    if (num_addrs != mapped_addrs_count) {
      return ZX_ERR_INVALID_ARGS;
    }
    for (uint64_t i = 0; i < num_addrs; i++) {
      dev_vaddr_t mapped_addr = 0;
      size_t mapped_len = 0;
      // Expect at least min_contig length, but could be less if this is the last portion of the
      // mapping.
      const size_t expected_size = ktl::min(pinned_vmo_.size() - (min_contig * i), min_contig);
      zx_status_t status =
          bti_->iommu().QueryAddress(bti_id, pinned_vmo_.vmo(), map_token_, (min_contig * i),
                                     expected_size, &mapped_addr, &mapped_len);
      if (status != ZX_OK) {
        return status;
      }
      ASSERT(mapped_len == expected_size);
      mapped_addrs[i] = mapped_addr;
    }
  } else if (contiguous) {
    if (mapped_addrs_count != 1 || !pinned_vmo_.vmo()->is_contiguous()) {
      return ZX_ERR_INVALID_ARGS;
    }
    dev_vaddr_t mapped_addr = 0;
    size_t mapped_len = 0;
    zx_status_t status = bti_->iommu().QueryAddress(bti_id, pinned_vmo_.vmo(), map_token_, 0,
                                                    pinned_vmo_.size(), &mapped_addr, &mapped_len);
    if (status != ZX_OK) {
      return status;
    }
    ASSERT(mapped_len == pinned_vmo_.size());
    *mapped_addrs = mapped_addr;
  } else {
    const size_t num_pages = pinned_vmo_.size() / PAGE_SIZE;
    if (num_pages != mapped_addrs_count) {
      return ZX_ERR_INVALID_ARGS;
    }
    for (size_t i = 0; i < num_pages; ++i) {
      dev_vaddr_t mapped_addr = 0;
      size_t mapped_len = 0;
      zx_status_t status =
          bti_->iommu().QueryAddress(bti_id, pinned_vmo_.vmo(), map_token_, i * PAGE_SIZE,
                                     PAGE_SIZE, &mapped_addr, &mapped_len);
      if (status != ZX_OK) {
        return status;
      }
      ASSERT(mapped_len == PAGE_SIZE);
      mapped_addrs[i] = mapped_addr;
    }
  }
  return ZX_OK;
}
