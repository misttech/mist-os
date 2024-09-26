// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_RODSO_H_
#define ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_RODSO_H_

#include <align.h>

#include <ktl/string_view.h>
#include <object/handle.h>
#include <object/vm_object_dispatcher.h>

// An RoDso object describes one DSO image built with the rodso.ld layout.
class RoDso {
 public:
  RoDso(fbl::RefPtr<VmObject> vmo, size_t size, uintptr_t code_start)
      : vmo_(ktl::move(vmo)), size_(size), code_start_(code_start) {
    DEBUG_ASSERT(size > 0);
    DEBUG_ASSERT(IS_PAGE_ALIGNED(size));
    DEBUG_ASSERT(code_start > 0);
    DEBUG_ASSERT(IS_PAGE_ALIGNED(code_start));
  }

  const fbl::RefPtr<VmObject>& vmo() const { return vmo_; }

  size_t size() const { return size_; }

  bool valid_code_mapping(uint64_t vmo_offset, size_t code_size) const {
    return vmo_offset == code_start_ && code_size == size_ - code_start_;
  }

  zx_status_t Map(fbl::RefPtr<VmAddressRegionDispatcher> vmar, size_t offset) const;

 private:
  zx_status_t MapSegment(fbl::RefPtr<VmAddressRegionDispatcher> vmar, bool code, size_t vmar_offset,
                         size_t start_offset, size_t end_offset) const;

  fbl::RefPtr<VmObject> vmo_;
  size_t size_;
  uintptr_t code_start_;
};

#endif  // ZIRCON_KERNEL_LIB_USERABI_INCLUDE_LIB_USERABI_RODSO_H_
