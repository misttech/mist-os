// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "vm/compressor.h"

#include <vm/compression.h>
#include <vm/physmap.h>

VmCompressor::~VmCompressor() {
  // Should not have an in progress compression.
  ASSERT(IsIdle());
  ASSERT(!IsTempReferenceInUse());
}

zx_status_t VmCompressor::Arm() {
  ASSERT(IsIdle());
  if (!spare_page_) {
    // Allocate a new one. Explicitly do not use delayed allocations.
    zx_status_t status = pmm_alloc_page(0, &spare_page_);
    if (status != ZX_OK) {
      return status;
    }
  }
  state_ = State::Ready;
  return ZX_OK;
}

VmCompressor::CompressedRef VmCompressor::Start(vm_page_t* page) {
  ASSERT(state_ == State::Ready);
  ASSERT(spare_page_);

  using_temp_reference_ = true;
  page_ = page;
  state_ = State::Started;
  return CompressedRef{temp_reference_};
}

VmCompressor::CompressResult VmCompressor::Compress() {
  ASSERT(state_ == State::Started);
  void* addr = paddr_to_physmap(page_->paddr());
  ASSERT(addr);
  VmCompressor::CompressResult result = compressor_.Compress(addr);
  state_ = State::Compressed;
  return result;
}

void VmCompressor::Finalize() {
  ASSERT(state_ == State::Compressed);
  // The temporary reference must no longer be in use.
  ASSERT(!IsTempReferenceInUse());
  state_ = State::Finalized;
}

void VmCompressor::Free(CompressedRef ref) {
  // Forbid returning the temporary reference this way.
  ASSERT(!IsTempReference(ref));
  compressor_.Free(ref);
}

void VmCompressor::ReturnTempReference(CompressedRef ref) {
  ASSERT(IsTempReference(ref));
  ASSERT(using_temp_reference_);
  using_temp_reference_ = false;
}
