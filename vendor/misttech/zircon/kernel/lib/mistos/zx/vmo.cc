// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/zx/vmo.h"

#include <trace.h>

#include <object/process_dispatcher.h>
#include <object/vm_object_dispatcher.h>

#define LOCAL_TRACE 0

namespace zx {

zx_status_t vmo::create(uint64_t size, uint32_t options, vmo* out) {
  LTRACEF("size %#" PRIx64 "\n", size);

  zx_status_t res;
  bool is_user_space = ThreadDispatcher::GetCurrent() != nullptr;
  if (is_user_space) {
    auto up = ProcessDispatcher::GetCurrent();
    res = up->EnforceBasicPolicy(ZX_POL_NEW_VMO);
    if (res != ZX_OK) {
      return res;
    }
  }

  zx::result<VmObjectDispatcher::CreateStats> parse_result =
      VmObjectDispatcher::parse_create_syscall_flags(options, size);
  if (parse_result.is_error()) {
    return parse_result.error_value();
  }
  VmObjectDispatcher::CreateStats stats = parse_result.value();

  // create a vm object
  fbl::RefPtr<VmObjectPaged> vmo;
  res = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY | (is_user_space ? PMM_ALLOC_FLAG_CAN_WAIT : 0),
                              stats.flags, stats.size, &vmo);
  if (res != ZX_OK)
    return res;

  // create a Vm Object dispatcher
  KernelHandle<VmObjectDispatcher> kernel_handle;
  zx_rights_t rights;
  zx_status_t result = VmObjectDispatcher::Create(ktl::move(vmo), size,
                                                  VmObjectDispatcher::InitialMutability::kMutable,
                                                  &kernel_handle, &rights);
  if (result != ZX_OK)
    return result;

  HandleOwner handle = Handle::Make(ktl::move(kernel_handle), rights);
  if (!handle) {
    return ZX_ERR_NO_MEMORY;
  }

  out->reset(ktl::move(handle));

  return ZX_OK;
}

zx_status_t vmo::read(void* data, uint64_t offset, size_t len) const {
  fbl::RefPtr<Dispatcher> dispatcher = handle_->dispatcher();
  auto vmo = DownCastDispatcher<VmObjectDispatcher>(&dispatcher);
  return vmo->vmo()->Read(data, offset, len);
}

zx_status_t vmo::write(const void* data, uint64_t offset, size_t len) const {
  fbl::RefPtr<Dispatcher> dispatcher = handle_->dispatcher();
  auto vmo = DownCastDispatcher<VmObjectDispatcher>(&dispatcher);
  return vmo->vmo()->Write(data, offset, len);
}

zx_status_t vmo::transfer_data(uint32_t options, uint64_t offset, uint64_t length, vmo* src_vmo,
                               uint64_t src_offset) {
  if (options) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!IS_PAGE_ALIGNED(offset) || !IS_PAGE_ALIGNED(length) || !IS_PAGE_ALIGNED(src_offset)) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!handle_->HasRights(ZX_RIGHT_WRITE)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  fbl::RefPtr<Dispatcher> dst_dispatcher = handle_->dispatcher();
  fbl::RefPtr<VmObjectDispatcher> dst_vmo_dispatcher =
      DownCastDispatcher<VmObjectDispatcher>(&dst_dispatcher);

  if (!src_vmo->get()->HasRights(ZX_RIGHT_READ | ZX_RIGHT_WRITE)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  fbl::RefPtr<Dispatcher> src_dispatcher = src_vmo->get()->dispatcher();
  fbl::RefPtr<VmObjectDispatcher> src_vmo_dispatcher =
      DownCastDispatcher<VmObjectDispatcher>(&src_dispatcher);

  // Short circuit out if src_vmo and dst_vmo are identical and the src_offset is the same as
  // the destination offset.
  if (src_vmo_dispatcher->get_koid() == dst_vmo_dispatcher->get_koid() && src_offset == offset) {
    return ZX_OK;
  }

  VmPageSpliceList pages;
  zx_status_t status = src_vmo_dispatcher->vmo()->TakePages(src_offset, length, &pages);
  if (status != ZX_OK) {
    return status;
  }

  // TODO(https://fxbug.dev/42082399): Stop decompressing compressed pages from the source range.
  return dst_vmo_dispatcher->vmo()->SupplyPages(offset, length, &pages,
                                                SupplyOptions::TransferData);
}

zx_status_t vmo::get_size(uint64_t* size) const {
  LTRACEF("handle %p, sizep %p\n", handle_, size);

  fbl::RefPtr<Dispatcher> dispatcher = handle_->dispatcher();
  auto vmo = DownCastDispatcher<VmObjectDispatcher>(&dispatcher);

  return vmo->GetSize(size);
}

zx_status_t vmo::set_size(uint64_t size) const {
  LTRACEF("handle %p, size %#" PRIx64 "\n", handle_, size);

  if (!handle_->HasRights(ZX_RIGHT_WRITE)) {
    return ZX_ERR_ACCESS_DENIED;
  }

  fbl::RefPtr<Dispatcher> dispatcher = handle_->dispatcher();
  auto vmo = DownCastDispatcher<VmObjectDispatcher>(&dispatcher);

  // VMOs that are not resizable should fail with ZX_ERR_UNAVAILABLE for backwards compatibility,
  // which will be handled by the SetSize call below. Only validate the RESIZE right if the VMO is
  // resizable.
  if (vmo->vmo()->is_resizable() && (handle_->rights() & ZX_RIGHT_RESIZE) == 0) {
    return ZX_ERR_ACCESS_DENIED;
  }
  // do the operation
  return vmo->SetSize(size);
}

zx_status_t vmo::create_child(uint32_t options, uint64_t offset, uint64_t size, vmo* out) const {
  LTRACEF("handle %p options %#x offset %#" PRIx64 " size %#" PRIx64 "\n", handle_, options, offset,
          size);

  zx_status_t status;
  fbl::RefPtr<VmObject> child_vmo;
  bool no_write = false;

  uint64_t vmo_size = 0;
  status = VmObject::RoundSize(size, &vmo_size);
  if (status != ZX_OK) {
    return status;
  }

  // Resizing a VMO requires the WRITE permissions, but NO_WRITE forbids the WRITE permissions, as
  // such it does not make sense to create a VMO with both of these.
  if ((options & ZX_VMO_CHILD_NO_WRITE) && (options & ZX_VMO_CHILD_RESIZABLE)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Writable is a property of the handle, not the object, so we consume this option here before
  // calling CreateChild.
  if (options & ZX_VMO_CHILD_NO_WRITE) {
    no_write = true;
    options &= ~ZX_VMO_CHILD_NO_WRITE;
  }

  // lookup the dispatcher from handle, save a copy of the rights for later. We must hold onto
  // the refptr of this VMO up until we create the dispatcher. The reason for this is that
  // VmObjectDispatcher::Create sets the user_id and page_attribution_id in the created child
  // vmo. Should the vmo destroyed between creating the child and setting the id in the dispatcher
  // the currently unset user_id may be used to re-attribute a parent. Holding the refptr prevents
  // any destruction from occurring.
  if (!handle_->HasRights(ZX_RIGHT_DUPLICATE | ZX_RIGHT_READ)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  fbl::RefPtr<Dispatcher> dispatcher = handle_->dispatcher();
  auto vmo = DownCastDispatcher<VmObjectDispatcher>(&dispatcher);
  zx_rights_t in_rights = handle_->rights();

  // clone the vmo into a new one
  status =
      vmo->CreateChild(options, offset, vmo_size, in_rights & ZX_RIGHT_GET_PROPERTY, &child_vmo);
  if (status != ZX_OK)
    return status;

  DEBUG_ASSERT(child_vmo);

  // This checks that the child VMO is explicitly created with ZX_VMO_CHILD_SNAPSHOT.
  // There are other ways that VMOs can be effectively immutable, for instance if the VMO is
  // created with ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE and meets certain criteria it will be
  // "upgraded" to a snapshot. However this behavior is not guaranteed at the API level.
  // A choice was made to conservatively only mark VMOs as immutable when the user explicitly
  // creates a VMO in a way that is guaranteed at the API level to always output an immutable VMO.
  auto initial_mutability = VmObjectDispatcher::InitialMutability::kMutable;
  if (no_write && (options & ZX_VMO_CHILD_SNAPSHOT)) {
    initial_mutability = VmObjectDispatcher::InitialMutability::kImmutable;
  }

  // create a Vm Object dispatcher
  KernelHandle<VmObjectDispatcher> kernel_handle;
  zx_rights_t default_rights;

  // A reference child shares the same content size manager as the parent.
  if (options & ZX_VMO_CHILD_REFERENCE) {
    auto result = vmo->content_size_manager();
    if (result.is_error()) {
      return result.status_value();
    }
    status = VmObjectDispatcher::CreateWithCsm(ktl::move(child_vmo), ktl::move(*result),
                                               initial_mutability, &kernel_handle, &default_rights);
  } else {
    status = VmObjectDispatcher::Create(ktl::move(child_vmo), size, initial_mutability,
                                        &kernel_handle, &default_rights);
  }
  if (status != ZX_OK) {
    return status;
  }

  // Set the rights to the new handle to no greater than the input (parent) handle minus the RESIZE
  // right, which is added independently based on ZX_VMO_CHILD_RESIZABLE; it is possible for a
  // non-resizable parent to have a resizable child and vice versa. Always allow GET/SET_PROPERTY so
  // the user can set ZX_PROP_NAME on the new clone.
  zx_rights_t rights = (in_rights & ~ZX_RIGHT_RESIZE) |
                       (options & ZX_VMO_CHILD_RESIZABLE ? ZX_RIGHT_RESIZE : 0) |
                       ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_SET_PROPERTY;

  // Unless it was explicitly requested to be removed, WRITE can be added to CoW clones at the
  // expense of executability.
  if (no_write) {
    rights &= ~ZX_RIGHT_WRITE;
    // NO_WRITE and RESIZABLE cannot be specified together, so we should not have the RESIZE
    // right.
    DEBUG_ASSERT((rights & ZX_RIGHT_RESIZE) == 0);
  } else if (options & (ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE |
                        ZX_VMO_CHILD_SNAPSHOT_MODIFIED)) {
    rights &= ~ZX_RIGHT_EXECUTE;
    rights |= ZX_RIGHT_WRITE;
  }

  // make sure we're somehow not elevating rights beyond what a new vmo should have
  DEBUG_ASSERT(((default_rights | ZX_RIGHT_EXECUTE) & rights) == rights);

  HandleOwner handle = Handle::Make(ktl::move(kernel_handle), rights);
  if (!handle) {
    return ZX_ERR_NO_MEMORY;
  }

  out->reset(ktl::move(handle));

  return ZX_OK;
}

zx_status_t vmo::op_range(uint32_t op, uint64_t offset, uint64_t size, void* buffer,
                          size_t buffer_size) const {
  LTRACEF("handle %p op %u offset %#" PRIx64 " size %#" PRIx64 " buffer %p buffer_size %zu\n",
          handle_, op, offset, size, buffer, buffer_size);

  fbl::RefPtr<Dispatcher> dispatcher = handle_->dispatcher();
  auto vmo = DownCastDispatcher<VmObjectDispatcher>(&dispatcher);

  return vmo->RangeOp(op, offset, size, user_inout_ptr<void>(buffer), buffer_size,
                      handle_->rights());
}

zx_status_t vmo::replace_as_executable(const resource& vmex, vmo* result) {
  HandleOwner handle = Handle::Make(handle_->dispatcher(), (handle_->rights() | ZX_RIGHT_EXECUTE));
  // We store ZX_HANDLE_INVALID to value_ before calling reset on result
  // in case result == this.
  handle_owner_.release();
  handle_ = nullptr;
  result->reset(ktl::move(handle));
  return ZX_OK;
}

}  // namespace zx
