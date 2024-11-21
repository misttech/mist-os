// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/zx/vmar.h"

#include <trace.h>
#include <zircon/features.h>

#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>

namespace zx {

namespace {
zx_status_t vmar_map_common(zx_vm_option_t options, fbl::RefPtr<VmAddressRegionDispatcher> vmar,
                            uint64_t vmar_offset, zx_rights_t vmar_rights,
                            fbl::RefPtr<VmObject> vmo, uint64_t vmo_offset, zx_rights_t vmo_rights,
                            uint64_t len, zx_vaddr_t* mapped_addr) {
  // Test to see if we should even be able to map this.
  if (!(vmo_rights & ZX_RIGHT_MAP)) {
    return ZX_ERR_ACCESS_DENIED;
  }

  if ((options & ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED)) {
    if (!(arch_vm_features() & ZX_VM_FEATURE_CAN_MAP_XOM)) {
      options |= ZX_VM_PERM_READ;
    }
  }

  if (!VmAddressRegionDispatcher::is_valid_mapping_protection(options)) {
    return ZX_ERR_INVALID_ARGS;
  }

  bool do_map_range = false;
  if (options & ZX_VM_MAP_RANGE) {
    do_map_range = true;
    options &= ~ZX_VM_MAP_RANGE;
  }

  if (do_map_range && (options & ZX_VM_SPECIFIC_OVERWRITE)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Usermode is not allowed to specify these flags on mappings, though we may
  // set them below.
  if (options & (ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE | ZX_VM_CAN_MAP_EXECUTE)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Permissions allowed by both the VMO and the VMAR.
  const bool can_read = (vmo_rights & ZX_RIGHT_READ) && (vmar_rights & ZX_RIGHT_READ);
  const bool can_write = (vmo_rights & ZX_RIGHT_WRITE) && (vmar_rights & ZX_RIGHT_WRITE);
  const bool can_exec = (vmo_rights & ZX_RIGHT_EXECUTE) && (vmar_rights & ZX_RIGHT_EXECUTE);

  // Test to see if the requested mapping protections are allowed.
  if ((options & ZX_VM_PERM_READ) && !can_read) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if ((options & ZX_VM_PERM_WRITE) && !can_write) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if ((options & ZX_VM_PERM_EXECUTE) && !can_exec) {
    return ZX_ERR_ACCESS_DENIED;
  }

  // If a permission is allowed by both the VMO and the VMAR, add it to the
  // flags for the new mapping, so that the VMO's rights as of now can be used
  // to constrain future permission changes via Protect().
  if (can_read) {
    options |= ZX_VM_CAN_MAP_READ;
  }
  if (can_write) {
    options |= ZX_VM_CAN_MAP_WRITE;
  }
  if (can_exec) {
    options |= ZX_VM_CAN_MAP_EXECUTE;
  }

  zx::result<VmAddressRegionDispatcher::MapResult> map_result =
      vmar->Map(vmar_offset, ktl::move(vmo), vmo_offset, len, options);
  if (map_result.is_error()) {
    return map_result.status_value();
  }

  // Setup a handler to destroy the new mapping if the syscall is unsuccessful.
  auto cleanup_handler = fit::defer([&map_result]() { map_result->mapping->Destroy(); });

  if (do_map_range) {
    // Mappings may have already been created due to memory priority, so need to ignore existing.
    // Ignoring existing mappings is safe here as we are always free to populate and destroy page
    // table mappings for user addresses.
    zx_status_t status =
        map_result->mapping->MapRange(0, len, /*commit=*/false, /*ignore_existing=*/true);
    if (status != ZX_OK) {
      return status;
    }
  }

  *mapped_addr = map_result->base;

  cleanup_handler.cancel();

  // This mapping will now always be used via the aspace so it is free to be merged into different
  // actual mapping objects.
  VmMapping::MarkMergeable(ktl::move(map_result->mapping));

  return ZX_OK;
}
}  // namespace

zx_status_t vmar::map(zx_vm_option_t options, size_t vmar_offset, const vmo& vmo_handle,
                      uint64_t vmo_offset, size_t len, zx_vaddr_t* ptr) const {
  fbl::RefPtr<Dispatcher> vmar_dispatcher = handle_->dispatcher();
  fbl::RefPtr<VmAddressRegionDispatcher> vmar =
      DownCastDispatcher<VmAddressRegionDispatcher>(&vmar_dispatcher);

  auto handle = vmo_handle.get();
  fbl::RefPtr<Dispatcher> vmo_dispatcher = handle->dispatcher();
  auto vmo = DownCastDispatcher<VmObjectDispatcher>(&vmo_dispatcher);

  return vmar_map_common(options, ktl::move(vmar), vmar_offset, handle_->rights(), vmo->vmo(),
                         vmo_offset, handle->rights(), len, ptr);
}

zx_status_t vmar::unmap(uintptr_t address, size_t len) const {
  fbl::RefPtr<Dispatcher> vmar_dispatcher = handle_->dispatcher();
  fbl::RefPtr<VmAddressRegionDispatcher> vmar =
      DownCastDispatcher<VmAddressRegionDispatcher>(&vmar_dispatcher);

  return vmar->Unmap(address, len,
                     VmAddressRegionDispatcher::op_children_from_rights(handle_->rights()));
}

zx_status_t vmar::protect(zx_vm_option_t options, uintptr_t addr, size_t len) const {
  if ((options & ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED)) {
    if (!(arch_vm_features() & ZX_VM_FEATURE_CAN_MAP_XOM)) {
      options |= ZX_VM_PERM_READ;
    }
  }

  zx_rights_t vmar_rights = 0u;
  if (options & ZX_VM_PERM_READ) {
    vmar_rights |= ZX_RIGHT_READ;
  }
  if (options & ZX_VM_PERM_WRITE) {
    vmar_rights |= ZX_RIGHT_WRITE;
  }
  if (options & ZX_VM_PERM_EXECUTE) {
    vmar_rights |= ZX_RIGHT_EXECUTE;
  }

  fbl::RefPtr<Dispatcher> vmar_dispatcher = handle_->dispatcher();
  fbl::RefPtr<VmAddressRegionDispatcher> vmar =
      DownCastDispatcher<VmAddressRegionDispatcher>(&vmar_dispatcher);

  if (!handle_->HasRights(vmar_rights)) {
    return ZX_ERR_ACCESS_DENIED;
  }

  if (!VmAddressRegionDispatcher::is_valid_mapping_protection(options)) {
    return ZX_ERR_INVALID_ARGS;
  }

  return vmar->Protect(addr, len, options,
                       VmAddressRegionDispatcher::op_children_from_rights(vmar_rights));
}

zx_status_t vmar::op_range(uint32_t op, uint64_t offset, uint64_t size, void* buffer,
                           size_t buffer_size) const {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t vmar::destroy() const {
  if (!handle_->HasRights(ZX_RIGHT_OP_CHILDREN)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  fbl::RefPtr<Dispatcher> dispatcher = handle_->dispatcher();
  fbl::RefPtr<VmAddressRegionDispatcher> vmar =
      DownCastDispatcher<VmAddressRegionDispatcher>(&dispatcher);

  return vmar->Destroy();
}

zx_status_t vmar::allocate(uint32_t options, size_t offset, size_t size, vmar* child,
                           uintptr_t* child_addr) const {
  // Compute needed rights from requested mapping protections.
  zx_rights_t vmar_rights = 0u;
  if (options & ZX_VM_CAN_MAP_READ) {
    vmar_rights |= ZX_RIGHT_READ;
  }
  if (options & ZX_VM_CAN_MAP_WRITE) {
    vmar_rights |= ZX_RIGHT_WRITE;
  }
  if (options & ZX_VM_CAN_MAP_EXECUTE) {
    vmar_rights |= ZX_RIGHT_EXECUTE;
  }

  if (!handle_->HasRights(vmar_rights)) {
    return ZX_ERR_ACCESS_DENIED;
  }

  fbl::RefPtr<Dispatcher> dispatcher = handle_->dispatcher();
  fbl::RefPtr<VmAddressRegionDispatcher> vmar =
      DownCastDispatcher<VmAddressRegionDispatcher>(&dispatcher);

  KernelHandle<VmAddressRegionDispatcher> kernel_handle;
  zx_rights_t new_rights;
  zx_status_t status = vmar->Allocate(offset, size, options, &kernel_handle, &new_rights);
  if (status != ZX_OK) {
    return status;
  }

  // Setup a handler to destroy the new VMAR if is unsuccessful.
  fbl::RefPtr<VmAddressRegionDispatcher> vmar_dispatcher = kernel_handle.dispatcher();
  auto cleanup_handler = fit::defer([&vmar_dispatcher]() { vmar_dispatcher->Destroy(); });

  // Create a handle and attach the dispatcher to it
  HandleOwner handle = Handle::Make(ktl::move(kernel_handle), new_rights);
  if (!handle) {
    return ZX_ERR_NO_MEMORY;
  }
  child->reset(ktl::move(handle));

  *child_addr = vmar_dispatcher->vmar()->base();

  if (status == ZX_OK) {
    cleanup_handler.cancel();
  }
  return status;
}

}  // namespace zx
