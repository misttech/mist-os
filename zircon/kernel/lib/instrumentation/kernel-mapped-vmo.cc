// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "kernel-mapped-vmo.h"

#include <align.h>

#include <ktl/move.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_aspace.h>

#include <ktl/enforce.h>

zx_status_t KernelMappedVmo::Init(fbl::RefPtr<VmObject> vmo, size_t offset, size_t size,
                                  const char* name) {
  ZX_ASSERT(offset % PAGE_SIZE == 0);
  size = ROUNDUP_PAGE_SIZE(size);
  zx_status_t status =
      PinnedVmObject::Create(ktl::move(vmo), offset, size, /*write=*/true, &pinned_vmo_);
  if (status == ZX_OK) {
    status = VmAspace::kernel_aspace()->RootVmar()->CreateVmMapping(
        0, size, 0, VMAR_FLAG_CAN_MAP_READ | VMAR_FLAG_CAN_MAP_WRITE, pinned_vmo_.vmo(), offset,
        ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE, name, &mapping_);
  }
  if (status == ZX_OK) {
    status = mapping_->MapRange(0, size, true);
  }
  return status;
}

KernelMappedVmo::~KernelMappedVmo() {
  if (mapping_) {
    mapping_->Destroy();
    // pinned_vmo_'s destructor will un-pin the pages just unmapped.
  }
}

Handle* KernelMappedVmo::Publish(ktl::string_view vmo_name, size_t content_size) {
  fbl::RefPtr<ContentSizeManager> content_size_manager;
  zx_status_t status = ContentSizeManager::Create(content_size, &content_size_manager);
  ZX_ASSERT(status == ZX_OK);

  zx_rights_t rights;
  KernelHandle<VmObjectDispatcher> handle;
  status =
      VmObjectDispatcher::Create(pinned_vmo_.vmo(), ktl::move(content_size_manager),
                                 VmObjectDispatcher::InitialMutability::kMutable, &handle, &rights);
  ZX_ASSERT(status == ZX_OK);
  status = handle.dispatcher()->set_name(vmo_name.data(), vmo_name.size());
  DEBUG_ASSERT(status == ZX_OK);
  return Handle::Make(ktl::move(handle), rights & ~ZX_RIGHT_WRITE).release();
}
