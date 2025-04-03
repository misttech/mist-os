// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <lib/fzl/pinned-vmo.h>
#include <lib/zx/result.h>

#include <limits>
#include <memory>

#include <ktl/array.h>
#include <object/bus_transaction_initiator_dispatcher.h>
#include <object/vm_object_dispatcher.h>

namespace fzl {

namespace {
// Helper for optimizing writing many small elements of user ptr array by allowing for a variable
// amount of buffering.
template <typename T, size_t Buf>
class BufferedUserOutPtr {
 public:
  explicit BufferedUserOutPtr(T* out_ptr) : out_ptr_(out_ptr) {}
  ~BufferedUserOutPtr() {
    // Ensure Flush was called and everything got written out.
    ZX_ASSERT(index_ == 0);
  }
  // Add a single element, either appending to the buffer and/or flushing the buffer if full.
  zx_status_t Write(const T& item) {
    buf_[index_] = item;
    index_++;
    if (index_ == Buf) {
      return Flush();
    }
    return ZX_OK;
  }
  // Flush any remaining buffered items. Must be called prior to destruction.
  zx_status_t Flush() {
    // zx_status_t status = out_ptr_.copy_array_to_user(buf_.data(), index_);
    // if (status != ZX_OK) {
    //   return status;
    // }
    memcpy(out_ptr_, buf_.data(), index_ * sizeof(T));
    out_ptr_ += index_;
    index_ = 0;
    return ZX_OK;
  }

 private:
  size_t index_ = 0;
  T* out_ptr_;
  ktl::array<T, Buf> buf_;
  // Expectation is this is going to be stack allocated, so ensure it's not too big.
  static_assert(sizeof(buf_) < PAGE_SIZE);
};
}  // namespace

zx_status_t PinnedVmo::Pin(const fbl::RefPtr<VmObjectDispatcher>& vmo,
                           const fbl::RefPtr<BusTransactionInitiatorDispatcher>& bti,
                           uint32_t options) {
  if (!vmo) {
    return ZX_ERR_INVALID_ARGS;
  }
  // To pin the entire VMO, we need to get the length:
  zx_status_t res;
  uint64_t vmo_size;
  res = vmo->GetSize(&vmo_size);
  if (res != ZX_OK) {
    return res;
  }
  return PinInternal(0, vmo_size, vmo, bti, options);
}

zx_status_t PinnedVmo::PinRange(uint64_t offset, uint64_t len,
                                const fbl::RefPtr<VmObjectDispatcher>& vmo,
                                const fbl::RefPtr<BusTransactionInitiatorDispatcher>& bti,
                                uint32_t options) {
  const size_t kPageSize = PAGE_SIZE;
  if ((len & (kPageSize - 1)) || (offset & (kPageSize - 1)) || len == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  return PinInternal(offset, len, vmo, bti, options);
}

zx_status_t PinnedVmo::PinInternal(uint64_t offset, uint64_t len,
                                   const fbl::RefPtr<VmObjectDispatcher>& vmo,
                                   const fbl::RefPtr<BusTransactionInitiatorDispatcher>& bti,
                                   uint32_t options) {
  const size_t kPageSize = PAGE_SIZE;
  zx_status_t res;

  // If we are holding a pinned memory token, then we are already holding a
  // pinned VMO.  It is an error to try and pin a new VMO without first
  // explicitly unpinning the old one.
  if (pmt_.dispatcher()) {
    ZX_DEBUG_ASSERT(regions_ != nullptr);
    ZX_DEBUG_ASSERT(region_count_ > 0);
    return ZX_ERR_BAD_STATE;
  }

  // Check our args, read/write/bti_contiguous is all that users may ask for.
  constexpr uint32_t kAllowedOptions = ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE | ZX_BTI_CONTIGUOUS;
  if (((options & kAllowedOptions) != options) /*|| !vmo.is_valid() || !bti.is_valid()*/) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Allocate storage for the results.
  ZX_DEBUG_ASSERT((len > 0) && !(len & (kPageSize - 1)));
  ZX_DEBUG_ASSERT((len / kPageSize) < std::numeric_limits<uint32_t>::max());
  ZX_DEBUG_ASSERT(!(offset & (kPageSize - 1)));
  fbl::AllocChecker ac;
  uint32_t page_count = static_cast<uint32_t>(len / kPageSize);
  if (options & ZX_BTI_CONTIGUOUS) {
    page_count = 1;
  }

  std::unique_ptr<zx_paddr_t[]> addrs(new (&ac) zx_paddr_t[page_count]);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  // Now actually pin the region.
  zx_rights_t rights;
  res = bti->Pin(vmo->vmo(), offset, len, options, &pmt_, &rights);
  if (res != ZX_OK) {
    return res;
  }

  // From here on out, if anything goes wrong, we need to make sure to clean
  // up.  Setup an autocall to take care of this for us.
  auto cleanup = fit::defer([&]() { UnpinInternal(); });

  static_assert(sizeof(uint64_t) == sizeof(zx_paddr_t), "mismatched types");
  BufferedUserOutPtr<zx_paddr_t, 32> buffered_addrs(addrs.get());

  // Define a helper lambda with some state that can fetch potentially large ranges from the PMT,
  // but return them gradually. This just serves as an optimization around repeatedly querying the
  // PMT for a range that it knows is contiguous, but where we need to fill out multiple addresses
  // for the user.
  struct {
    uint64_t offset = 0;
    uint64_t addr = 0;
    size_t remaining = 0;
  } consume_state;
  auto consume_addr = [&](size_t expected_contig) -> zx::result<uint64_t> {
    // If the remaining part of the mapping we have cannot satisfy
    if (expected_contig > consume_state.remaining) {
      uint64_t remain = len - consume_state.offset;
      zx_status_t status = pmt_.dispatcher()->QueryAddress(
          consume_state.offset, remain, &consume_state.addr, &consume_state.remaining);
      if (status != ZX_OK) {
        return zx::error(status);
      }
      if (expected_contig > consume_state.remaining) {
        // This happening suggests an error with contiguity calculations and/or the underlying IOMMU
        // implementation not reporting its contiguity correctly.
        // TODO: consider making this a louder error.
        return zx::error(ZX_ERR_INVALID_ARGS);
      }
    }
    const uint64_t ret = consume_state.addr;
    consume_state.offset += expected_contig;
    consume_state.addr += expected_contig;
    consume_state.remaining -= expected_contig;
    return zx::ok(ret);
  };

  // Based on the passed in options, determine what size chunks we are going to report to the user,
  // and how many of those there will be.
  size_t target_contig;
  size_t expected_addrs;
  if (options & ZX_BTI_CONTIGUOUS) {
    expected_addrs = 1;
    target_contig = len;
  } else {
    expected_addrs = page_count;
    target_contig = kPageSize;
  }
  if (page_count != expected_addrs) {
    return ZX_ERR_INVALID_ARGS;
  }
  // Calculate / lookup the addresses.
  for (size_t i = 0; i < page_count; i++) {
    const size_t expected_size = ktl::min(len - (target_contig * i), target_contig);
    auto result = consume_addr(expected_size);
    if (result.is_error()) {
      return result.status_value();
    }
    zx_status_t status = buffered_addrs.Write(*result);
    if (status != ZX_OK) {
      return status;
    }
  }

  zx_status_t status = buffered_addrs.Flush();
  if (status != ZX_OK) {
    return status;
  }

  if (options & ZX_BTI_CONTIGUOUS) {
    // We can do less work in the contiguous case.
    regions_.reset(new (&ac) Region[1]);
    if (!ac.check()) {
      return ZX_ERR_NO_MEMORY;
    }

    region_count_ = 1;
    regions_[0].phys_addr = addrs[0];
    // The region is the size of the entire vmo in the contiguous case.
    regions_[0].size = len;
    cleanup.cancel();
    return ZX_OK;
  }

  // Do a quick pass over the pages to figure out how many adjacent pages we
  // can merge.  This will let us know how many regions we will need storage
  // for our regions array.
  zx_paddr_t last = addrs[0];
  region_count_ = 1;
  for (uint32_t i = 1; i < page_count; ++i) {
    if (addrs[i] != (last + kPageSize)) {
      ++region_count_;
    }
    last = addrs[i];
  }

  // Allocate storage for our regions.
  regions_.reset(new (&ac) Region[region_count_]);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  // Finally, go ahead and merge any adjacent pages to compute our set of
  // regions and we should be good to go;
  regions_[0].phys_addr = addrs[0];
  regions_[0].size = kPageSize;
  for (uint32_t i = 1, j = 0; i < page_count; ++i) {
    ZX_DEBUG_ASSERT(j < region_count_);

    if ((regions_[j].phys_addr + regions_[j].size) == addrs[i]) {
      // Merge!
      regions_[j].size += kPageSize;
    } else {
      // New Region!
      ++j;
      ZX_DEBUG_ASSERT(j < region_count_);
      regions_[j].phys_addr = addrs[i];
      regions_[j].size = kPageSize;
    }
  }

  cleanup.cancel();
  return ZX_OK;
}

void PinnedVmo::Unpin() {
  if (!pmt_.dispatcher()) {
    ZX_DEBUG_ASSERT(regions_ == nullptr);
    ZX_DEBUG_ASSERT(region_count_ == 0);
    return;
  }

  ZX_DEBUG_ASSERT(regions_ != nullptr);
  ZX_DEBUG_ASSERT(region_count_ > 0);

  UnpinInternal();
}

void PinnedVmo::UnpinInternal() {
  ZX_DEBUG_ASSERT(pmt_.dispatcher());

  // Given the level of sanity checking we have done so far, it should be
  // completely impossible for us to fail to unpin this memory.
  // pmt_.dispatcher()->Unpin();
  // pinned_vmo_'s destructor will un-pin the pages just unmapped.
  pmt_.reset();

  regions_.reset();
  region_count_ = 0;
}

}  // namespace fzl
