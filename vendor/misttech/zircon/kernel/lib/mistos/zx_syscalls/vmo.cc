// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/fit/defer.h>
#include <lib/mistos/zx_syscalls/util.h>
#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <fbl/ref_ptr.h>
#include <object/handle.h>
#include <object/process_dispatcher.h>
#include <object/resource.h>
#include <object/resource_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_object.h>
#include <vm/vm_object_paged.h>

#define LOCAL_TRACE 0

static_assert(ZX_CACHE_POLICY_CACHED == ARCH_MMU_FLAG_CACHED,
              "Cache policy constant mismatch - CACHED");
static_assert(ZX_CACHE_POLICY_UNCACHED == ARCH_MMU_FLAG_UNCACHED,
              "Cache policy constant mismatch - UNCACHED");
static_assert(ZX_CACHE_POLICY_UNCACHED_DEVICE == ARCH_MMU_FLAG_UNCACHED_DEVICE,
              "Cache policy constant mismatch - UNCACHED_DEVICE");
static_assert(ZX_CACHE_POLICY_WRITE_COMBINING == ARCH_MMU_FLAG_WRITE_COMBINING,
              "Cache policy constant mismatch - WRITE_COMBINING");
static_assert(ZX_CACHE_POLICY_MASK == ARCH_MMU_FLAG_CACHE_MASK,
              "Cache policy constant mismatch - CACHE_MASK");

zx_status_t zx_vmo_create(uint64_t size, uint32_t options, zx_handle_t* out) {
  LTRACEF("size %#" PRIx64 "\n", size);

  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);

  zx::result<VmObjectDispatcher::CreateStats> parse_result =
      VmObjectDispatcher::parse_create_syscall_flags(options, size);
  if (parse_result.is_error()) {
    return parse_result.error_value();
  }
  VmObjectDispatcher::CreateStats stats = parse_result.value();

  // create a vm object
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t res =
      VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY | (is_user_thread ? PMM_ALLOC_FLAG_CAN_WAIT : 0),
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

  // create a handle and attach the dispatcher to it
  return MakeAndAddHandle(ktl::move(kernel_handle), rights, out);
}

zx_status_t zx_vmo_read(zx_handle_t handle, void* _data, uint64_t offset, size_t len) {
  LTRACEF("handle %x, data %p, offset %#" PRIx64 ", len %#zx\n", handle, _data, offset, len);

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  // lookup the dispatcher from handle
  fbl::RefPtr<VmObjectDispatcher> vmo;
  zx_status_t status = handle_table(up).GetDispatcherWithRights(*up, handle, ZX_RIGHT_READ, &vmo);
  if (status != ZX_OK) {
    LTRACEF("status %d\n", status);
    return status;
  }

  if (is_user_thread) {
    user_out_ptr<void> data(_data);
    return vmo->Read(data.reinterpret<char>(), len, offset, nullptr);
  } else {
    if (_data == nullptr) {
      // nullptr is not valid kernel address it will fail inside Write
      // but as len is zero it ok, we just do nothing.
      if (len == 0) {
        return ZX_OK;
      }
      return ZX_ERR_INVALID_ARGS;
    }
    return vmo->vmo()->Read(_data, offset, len);
  }
}

zx_status_t zx_vmo_write(zx_handle_t handle, const void* _data, uint64_t offset, size_t len) {
  LTRACEF("handle %x, data %p, offset %#" PRIx64 ", len %#zx\n", handle, _data, offset, len);

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  // lookup the dispatcher from handle
  fbl::RefPtr<VmObjectDispatcher> vmo;
  zx_status_t status = handle_table(up).GetDispatcherWithRights(*up, handle, ZX_RIGHT_WRITE, &vmo);
  if (status != ZX_OK) {
    LTRACEF("status %d\n", status);
    return status;
  }

  if (is_user_thread) {
    user_in_ptr<const void> data(_data);
    return vmo->Write(data.reinterpret<const char>(), len, offset, nullptr);
  } else {
    if (_data == nullptr) {
      // nullptr is not valid kernel address it will fail inside Write
      // but as len is zero it ok, we just do nothing.
      if (len == 0) {
        return ZX_OK;
      }
      return ZX_ERR_INVALID_ARGS;
    }
    return vmo->vmo()->Write(_data, offset, len);
  }
}

#if 0
// zx_status_t zx_vmo_transfer_data
zx_status_t sys_vmo_transfer_data(zx_handle_t dst_vmo_handle, uint32_t options, uint64_t offset,
                                  uint64_t length, zx_handle_t src_vmo_handle,
                                  uint64_t src_offset) {
  // Currently, there are no supported options. This may change in the future.
  if (options) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!IS_PAGE_ALIGNED(offset) || !IS_PAGE_ALIGNED(length) || !IS_PAGE_ALIGNED(src_offset)) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<VmObjectDispatcher> dst_vmo_dispatcher;
  zx_status_t status = up->handle_table().GetDispatcherWithRights(
      *up, dst_vmo_handle, ZX_RIGHT_WRITE, &dst_vmo_dispatcher);
  if (status != ZX_OK) {
    return status;
  }

  fbl::RefPtr<VmObjectDispatcher> src_vmo_dispatcher;
  status = up->handle_table().GetDispatcherWithRights(
      *up, src_vmo_handle, ZX_RIGHT_READ | ZX_RIGHT_WRITE, &src_vmo_dispatcher);
  if (status != ZX_OK) {
    return status;
  }

  // Short circuit out if src_vmo and dst_vmo are identical and the src_offset is the same as
  // the destination offset.
  if (src_vmo_dispatcher->get_koid() == dst_vmo_dispatcher->get_koid() && src_offset == offset) {
    return ZX_OK;
  }

  VmPageSpliceList pages;
  status = src_vmo_dispatcher->vmo()->TakePages(src_offset, length, &pages);
  if (status != ZX_OK) {
    return status;
  }

  // TODO(https://fxbug.dev/42082399): Stop decompressing compressed pages from the source range.
  return dst_vmo_dispatcher->vmo()->SupplyPages(offset, length, &pages,
                                                SupplyOptions::TransferData);
}
#endif

zx_status_t zx_vmo_get_size(zx_handle_t handle, uint64_t* _size) {
  LTRACEF("handle %x, sizep %p\n", handle, _size);

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  // lookup the dispatcher from handle
  fbl::RefPtr<VmObjectDispatcher> vmo;
  zx_status_t status = handle_table(up).GetDispatcher(*up, handle, &vmo);
  if (status != ZX_OK) {
    LTRACEF("status %d\n", status);
    return status;
  }

  // no rights check, anyone should be able to get the size

  // do the operation
  uint64_t size = 0;
  status = vmo->GetSize(&size);

  if (status != ZX_OK) {
    LTRACEF("status %d\n", status);
    return status;
  }

  *_size = size;
  return ZX_OK;
}

zx_status_t zx_vmo_set_size(zx_handle_t handle, uint64_t size) {
  LTRACEF("handle %x, size %#" PRIx64 "\n", handle, size);

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  // lookup the dispatcher from handle
  fbl::RefPtr<VmObjectDispatcher> vmo;
  zx_rights_t rights;
  zx_status_t status =
      handle_table(up).GetDispatcherWithRights(*up, handle, ZX_RIGHT_WRITE, &vmo, &rights);
  if (status != ZX_OK)
    return status;

  // VMOs that are not resizable should fail with ZX_ERR_UNAVAILABLE for backwards compatibility,
  // which will be handled by the SetSize call below. Only validate the RESIZE right if the VMO is
  // resizable.
  if (vmo->vmo()->is_resizable() && (rights & ZX_RIGHT_RESIZE) == 0) {
    return ZX_ERR_ACCESS_DENIED;
  }
  // do the operation
  return vmo->SetSize(size);
}

zx_status_t zx_vmo_op_range(zx_handle_t handle, uint32_t op, uint64_t offset, uint64_t size,
                            void* _buffer, size_t buffer_size) {
  LTRACEF("handle %x op %u offset %#" PRIx64 " size %#" PRIx64 " buffer %p buffer_size %zu\n",
          handle, op, offset, size, _buffer, buffer_size);

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  // lookup the dispatcher from handle
  // save the rights and pass down into the dispatcher for further testing
  fbl::RefPtr<VmObjectDispatcher> vmo;
  zx_rights_t rights;
  zx_status_t status = handle_table(up).GetDispatcherAndRights(*up, handle, &vmo, &rights);
  if (status != ZX_OK) {
    return status;
  }

  user_inout_ptr<void> ptr{_buffer};
  return vmo->RangeOp(op, offset, size, ptr, buffer_size, rights);
}

zx_status_t zx_vmo_set_cache_policy(zx_handle_t handle, uint32_t cache_policy) {
  fbl::RefPtr<VmObjectDispatcher> vmo;
  zx_status_t status = ZX_OK;
  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  // Sanity check the cache policy.
  if (cache_policy & ~ZX_CACHE_POLICY_MASK) {
    return ZX_ERR_INVALID_ARGS;
  }

  // lookup the dispatcher from handle.
  status = handle_table(up).GetDispatcherWithRights(*up, handle, ZX_RIGHT_MAP, &vmo);
  if (status != ZX_OK) {
    return status;
  }

  return vmo->SetMappingCachePolicy(cache_policy);
}

zx_status_t zx_vmo_create_child(zx_handle_t handle, uint32_t options, uint64_t offset,
                                uint64_t size, zx_handle_t* out_handle) {
  LTRACEF("handle %x options %#x offset %#" PRIx64 " size %#" PRIx64 "\n", handle, options, offset,
          size);

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

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
  fbl::RefPtr<VmObjectDispatcher> vmo;
  zx_rights_t in_rights;
  status = handle_table(up).GetDispatcherWithRights(*up, handle, ZX_RIGHT_DUPLICATE | ZX_RIGHT_READ,
                                                    &vmo, &in_rights);
  if (status != ZX_OK)
    return status;

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
    status = VmObjectDispatcher::CreateWithCsm(ktl::move(child_vmo),
                                               fbl::RefPtr(vmo->content_size_manager()),
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

  // create a handle and attach the dispatcher to it
  return MakeAndAddHandle(ktl::move(kernel_handle), rights, out_handle);
}

// Check if the resource referenced by |handle| is of kind |kind|, or ZX_RSRC_KIND_ROOT. If
// |kind| matches the resource's kind, then range validation between |base| and |size| will
// be made against the resource's backing address space allocation.
//
// Possible errors:
// ++ ZX_ERR_ACCESS_DENIED: |handle| is not a valid handle.
// ++ ZX_ERR_WRONG_TYPE: |handle| is not a valid resource handle, or |kind| is invalid for
//                       the request.
// ++ ZX_ERR_OUT_OF_RANGE: The range specified by |base| and |Len| is not granted by this
// resource.
zx_status_t validate_ranged_resource_mistos(zx_handle_t handle, zx_rsrc_kind_t kind, uintptr_t base,
                                            size_t size) {
  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  fbl::RefPtr<ResourceDispatcher> resource;
  auto status = handle_table(up).GetDispatcher(*up, handle, &resource);
  if (status != ZX_OK) {
    return status;
  }

  return validate_ranged_resource(resource, kind, base, size);
}

zx_status_t zx_vmo_replace_as_executable(zx_handle_t handle, zx_handle_t vmex, zx_handle_t* out) {
  LTRACEF("repexec %x %x\n", handle, vmex);

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  zx_status_t vmex_status = ZX_OK;
  if (vmex != ZX_HANDLE_INVALID) {
    vmex_status =
        validate_ranged_resource_mistos(vmex, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_VMEX_BASE, 1);
  } else {
    // vmex_status = up->EnforceBasicPolicy(ZX_POL_AMBIENT_MARK_VMO_EXEC);
  }

  HandleOwner orig_handle = handle_table(up).RemoveHandle(*up, handle);
  if (!orig_handle)
    return ZX_ERR_BAD_HANDLE;
  if (orig_handle->dispatcher()->get_type() != ZX_OBJ_TYPE_VMO)
    return ZX_ERR_BAD_HANDLE;

  if (vmex_status != ZX_OK)
    return vmex_status;

  return MakeAndAddHandle(orig_handle->dispatcher(), orig_handle->rights() | ZX_RIGHT_EXECUTE, out);
}
