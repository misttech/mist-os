// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/mm/memory.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/logging/logging.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/features.h>
#include <zircon/rights.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>
#include <ktl/span.h>
#include <object/handle.h>
#include <object/process_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_address_region.h>
#include <vm/vm_object.h>

#include "../kernel_priv.h"

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(2)

namespace {

// adapted from syscalls/vmar.cc
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

namespace starnix {

// Based on from sys_vmo_create (syscalls/vmo.cc)
fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> create_vmo(uint64_t size, uint32_t options) {
  LTRACEF("size %#" PRIx64 "\n", size);

  bool is_user_space = ThreadDispatcher::GetCurrent() != nullptr;

  if (is_user_space) {
    auto up = ProcessDispatcher::GetCurrent();
    zx_status_t res = up->EnforceBasicPolicy(ZX_POL_NEW_VMO);
    if (res != ZX_OK) {
      return fit::error(res);
    }
  }

  zx::result<VmObjectDispatcher::CreateStats> parse_result =
      VmObjectDispatcher::parse_create_syscall_flags(options, size);
  if (parse_result.is_error()) {
    return fit::error(parse_result.error_value());
  }
  VmObjectDispatcher::CreateStats stats = parse_result.value();

  // mremap can grow memory regions, so make sure the VMO is resizable.
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t status =
      VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY | (is_user_space ? PMM_ALLOC_FLAG_CAN_WAIT : 0),
                            stats.flags, stats.size, &vmo);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  // create a Vm Object dispatcher
  KernelHandle<VmObjectDispatcher> kernel_handle;
  zx_rights_t rights;
  zx_status_t result = VmObjectDispatcher::Create(ktl::move(vmo), size,
                                                  VmObjectDispatcher::InitialMutability::kMutable,
                                                  &kernel_handle, &rights);
  if (result != ZX_OK) {
    return fit::error(result);
  }
  return fit::ok(MemoryObject::From(Handle::Make(ktl::move(kernel_handle), rights)));
}

fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> create_child_vmo(const Vmo& vmo,
                                                                     uint32_t options,
                                                                     uint64_t offset,
                                                                     uint64_t size) {
  zx_status_t status;
  fbl::RefPtr<VmObject> child_vmo;
  bool no_write = false;

  uint64_t vmo_size = 0;
  status = VmObject::RoundSize(size, &vmo_size);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  // Resizing a VMO requires the WRITE permissions, but NO_WRITE forbids the WRITE
  // permissions, as
  // such it does not make sense to create a VMO with both of these.
  if ((options & ZX_VMO_CHILD_NO_WRITE) && (options & ZX_VMO_CHILD_RESIZABLE)) {
    return fit::error(ZX_ERR_INVALID_ARGS);
  }

  // Writable is a property of the handle, not the object, so we consume this option here
  // before calling CreateChild.
  if (options & ZX_VMO_CHILD_NO_WRITE) {
    no_write = true;
    options &= ~ZX_VMO_CHILD_NO_WRITE;
  }

  // lookup the dispatcher from handle, save a copy of the rights for later. We must hold
  // onto
  // the refptr of this VMO up until we create the dispatcher. The reason for this is that
  // VmObjectDispatcher::Create sets the user_id and page_attribution_id in the created
  // child vmo. Should the vmo destroyed between creating the child and setting the id in
  // the dispatcher the currently unset user_id may be used to re-attribute a parent.
  // Holding the refptr prevents any destruction from occurring.
  zx_rights_t in_rights = vmo.vmo->rights();
  if (!vmo.vmo->HasRights(ZX_RIGHT_DUPLICATE | ZX_RIGHT_READ)) {
    return fit::error(ZX_ERR_ACCESS_DENIED);
  }

  // clone the vmo into a new one
  status = vmo.dispatcher()->CreateChild(options, offset, size, in_rights & ZX_RIGHT_GET_PROPERTY,
                                         &child_vmo);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  DEBUG_ASSERT(child_vmo);

  // This checks that the child VMO is explicitly created with ZX_VMO_CHILD_SNAPSHOT.
  // There are other ways that VMOs can be effectively immutable, for instance if the VMO
  // is created with ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE and meets certain criteria it
  // will be "upgraded" to a snapshot. However this behavior is not guaranteed at the API
  // level. A choice was made to conservatively only mark VMOs as immutable when the user
  // explicitly creates a VMO in a way that is guaranteed at the API level to always
  // output an immutable VMO.
  auto initial_mutability = VmObjectDispatcher::InitialMutability::kMutable;
  if (no_write && (options & ZX_VMO_CHILD_SNAPSHOT)) {
    initial_mutability = VmObjectDispatcher::InitialMutability::kImmutable;
  }

  // create a Vm Object dispatcher
  KernelHandle<VmObjectDispatcher> kernel_handle;
  zx_rights_t default_rights;

  // A reference child shares the same content size manager as the parent.
  if (options & ZX_VMO_CHILD_REFERENCE) {
    auto result = vmo.dispatcher()->content_size_manager();
    if (result.is_error()) {
      return fit::error(result.status_value());
    }
    status = VmObjectDispatcher::CreateWithCsm(ktl::move(child_vmo), ktl::move(*result),
                                               initial_mutability, &kernel_handle, &default_rights);
  } else {
    status = VmObjectDispatcher::Create(ktl::move(child_vmo), size, initial_mutability,
                                        &kernel_handle, &default_rights);
  }

  // Set the rights to the new handle to no greater than the input (parent) handle minus
  // the RESIZE
  // right, which is added independently based on ZX_VMO_CHILD_RESIZABLE; it is possible
  // for a non-resizable parent to have a resizable child and vice versa. Always allow
  // GET/SET_PROPERTY so the user can set ZX_PROP_NAME on the new clone.
  zx_rights_t rights = (in_rights & ~ZX_RIGHT_RESIZE) |
                       (options & ZX_VMO_CHILD_RESIZABLE ? ZX_RIGHT_RESIZE : 0) |
                       ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_SET_PROPERTY;

  // Unless it was explicitly requested to be removed, WRITE can be added to CoW clones at
  // the expense of executability.
  if (no_write) {
    rights &= ~ZX_RIGHT_WRITE;
    // NO_WRITE and RESIZABLE cannot be specified together, so we should not have the
    // RESIZE right.
    DEBUG_ASSERT((rights & ZX_RIGHT_RESIZE) == 0);
  } else if (options & (ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE |
                        ZX_VMO_CHILD_SNAPSHOT_MODIFIED)) {
    rights &= ~ZX_RIGHT_EXECUTE;
    rights |= ZX_RIGHT_WRITE;
  }

  // make sure we're somehow not elevating rights beyond what a new vmo should have
  DEBUG_ASSERT(((default_rights | ZX_RIGHT_EXECUTE) & rights) == rights);

  return fit::ok(MemoryObject::From(Handle::Make(ktl::move(kernel_handle), rights)));
}

fbl::RefPtr<MemoryObject> MemoryObject::From(HandleOwner vmo) {
  fbl::AllocChecker ac;
  fbl::RefPtr<MemoryObject> memory = fbl::AdoptRef(new (&ac) MemoryObject(Vmo{ktl::move(vmo)}));
  ASSERT(ac.check());
  return ktl::move(memory);
}

ktl::optional<std::reference_wrapper<Vmo>> MemoryObject::as_vmo() {
  return ktl::visit(
      MemoryObject::overloaded{
          [](Vmo& vmo) { return ktl::optional<std::reference_wrapper<Vmo>>(vmo); },
          [](RingBuf&) { return ktl::optional<std::reference_wrapper<Vmo>>(ktl::nullopt); },
      },
      variant_);
}

ktl::optional<Vmo> MemoryObject::into_vmo() {
  return ktl::visit(MemoryObject::overloaded{
                        [](Vmo& vmo) { return ktl::optional<Vmo>(ktl::move(vmo)); },
                        [](RingBuf&) { return ktl::optional<Vmo>(ktl::nullopt); },
                    },
                    variant_);
}

uint64_t MemoryObject::get_content_size() const {
  return ktl::visit(MemoryObject::overloaded{
                        [](const Vmo& vmo) { return vmo.dispatcher()->GetContentSize(); },
                        [](const RingBuf&) { return 0ul; },
                    },
                    variant_);
}

void MemoryObject::set_content_size(uint64_t size) const {}

uint64_t MemoryObject::get_size() const {
  return ktl::visit(MemoryObject::overloaded{
                        [](const Vmo& vmo) {
                          uint64_t size;
                          auto status = vmo.dispatcher()->GetSize(&size);
                          if (status != ZX_OK) {
                            ZX_PANIC("failed to get vmo size %d", status);
                          }
                          return size;
                        },
                        [](const RingBuf&) { return 0ul; },
                    },
                    variant_);
}

fit::result<zx_status_t> MemoryObject::set_size(uint64_t size) const {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t> {
                          auto status = vmo.dispatcher()->SetSize(size);
                          if (status != ZX_OK) {
                            return fit::error(status);
                          }
                          return fit::ok();
                        },
                        [](const RingBuf&) -> fit::result<zx_status_t> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

// zx_vmo_create_child
fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> MemoryObject::create_child(uint32_t options,
                                                                               uint64_t offset,
                                                                               uint64_t size) {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
                          return create_child_vmo(vmo, options, offset, size);
                        },
                        [](const RingBuf&) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> MemoryObject::duplicate_handle(
    zx_rights_t rights) const {
  return ktl::visit(
      MemoryObject::overloaded{
          [&rights](const Vmo& vmo) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
            auto handle_ownwer = Handle::Dup(vmo.vmo.get(), rights);
            if (!handle_ownwer) {
              return fit::error(ZX_ERR_NO_MEMORY);
            }
            return fit::ok(MemoryObject::From(ktl::move(handle_ownwer)));
          },
          [](const RingBuf& buf) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
            return fit::error(ZX_ERR_NOT_SUPPORTED);
          },
      },
      variant_);
}

fit::result<zx_status_t> MemoryObject::read(ktl::span<uint8_t>& data, uint64_t offset) const {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t> {
                          /*if (!vmo.HasRights(ZX_RIGHT_READ))
                            return fit::error(ZX_ERR_ACCESS_DENIED);
                          user_out_ptr<void> _data =
                              make_user_out_ptr(reinterpret_cast<void*>(data.data()));
                          zx_status_t status = vmo.vmo->Read(_data.reinterpret<char>(), offset,
                                                             data.size(), nullptr);
                          }*/

                          zx_status_t status =
                              vmo.dispatcher()->vmo()->Read(data.data(), offset, data.size());
                          if (status != ZX_OK) {
                            LTRACEF("error on vmo read %d\n", status);
                            return fit::error(status);
                          }
                          return fit::ok();
                        },
                        [](const RingBuf& buf) -> fit::result<zx_status_t> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

fit::result<zx_status_t> MemoryObject::read_uninit(ktl::span<uint8_t>& data,
                                                   uint64_t offset) const {
  return MemoryObject::read(data, offset);
}

fit::result<zx_status_t, fbl::Vector<uint8_t>> MemoryObject::read_to_vec(uint64_t offset,
                                                                         uint64_t length) const {
  fbl::AllocChecker ac;
  fbl::Vector<uint8_t> buffer;
  buffer.reserve(length, &ac);
  if (!ac.check()) {
    return fit::error(ZX_ERR_NO_MEMORY);
  }
  ktl::span<uint8_t> data{buffer.data(), length};
  auto result = read_uninit(data, offset);
  if (result.is_error()) {
    return result.take_error();
  }
  {
    // SAFETY: since read_uninit succeeded we know that we can consider the buffer
    // initialized.
    buffer.set_size(length);
  }
  return fit::ok(ktl::move(buffer));
}

fit::result<zx_status_t> MemoryObject::write(const ktl::span<const uint8_t>& data,
                                             uint64_t offset) const {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t> {
                          /*if (!vmo.HasRights(ZX_RIGHT_WRITE))
                            return fit::error(ZX_ERR_ACCESS_DENIED);
                          user_in_ptr<const void> _data =
                              make_user_in_ptr(reinterpret_cast<const void*>(data.data()));
                          zx_status_t status = vmo.vmo->Write(_data.reinterpret<const char>(),
                                                              offset, data.size(), nullptr);*/

                          zx_status_t status =
                              vmo.dispatcher()->vmo()->Write(data.data(), offset, data.size());
                          if (status != ZX_OK) {
                            LTRACEF("error on vmo write %d\n", status);
                            return fit::error(status);
                          }
                          if (status != ZX_OK) {
                            return fit::error(status);
                          }
                          return fit::ok();
                        },
                        [](const RingBuf& buf) -> fit::result<zx_status_t> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

zx_info_handle_basic_t MemoryObject::basic_info() const {
  return ktl::visit(MemoryObject::overloaded{
                        [](const Vmo& vmo) {
                          return zx_info_handle_basic_t{
                              .koid = vmo.dispatcher()->get_koid(),
                              .rights = vmo.vmo->rights(),
                          };
                        },
                        [](const RingBuf& buf) {
                          return zx_info_handle_basic_t{
                              .koid = buf.dispatcher()->get_koid(),
                          };
                        },
                    },
                    variant_);
}

zx_info_vmo_t MemoryObject::info() const {
  zx_rights_t handle_rights;
  return ktl::visit(MemoryObject::overloaded{
                        [&handle_rights](const Vmo& vmo) -> zx_info_vmo_t {
                          return vmo.dispatcher()->GetVmoInfo(handle_rights);
                        },
                        [&handle_rights](const RingBuf& buf) -> zx_info_vmo_t {
                          return buf.dispatcher()->GetVmoInfo(handle_rights);
                        },
                    },
                    variant_);
}

void MemoryObject::set_zx_name(const char* name) const {
  ktl::visit(MemoryObject::overloaded{
                 [&name](const Vmo& vmo) {
                   ktl::span<const uint8_t> n{reinterpret_cast<const uint8_t*>(name), strlen(name)};
                   starnix::set_zx_name(vmo.dispatcher(), n);
                 },
                 [](const RingBuf&) {},
             },
             variant_);
}

fit::result<zx_status_t> MemoryObject::op_range(uint32_t op, uint64_t* offset,
                                                uint64_t* size) const {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t> {
                          zx_status_t status = vmo.dispatcher()->RangeOp(
                              op, *offset, *size, user_inout_ptr<void>(nullptr), 0,
                              vmo.vmo->rights());
                          if (status != ZX_OK) {
                            return fit::error(status);
                          }
                          return fit::ok();
                        },
                        [](const RingBuf&) -> fit::result<zx_status_t> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

// zx_vmo_replace_as_executable
fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> MemoryObject::replace_as_executable() {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
                          HandleOwner handle = Handle::Make(ktl::move(vmo.dispatcher()),
                                                            (vmo.vmo->rights() | ZX_RIGHT_EXECUTE));
                          return fit::ok(MemoryObject::From(ktl::move(handle)));
                        },
                        [](const RingBuf&) -> fit::result<zx_status_t, fbl::RefPtr<MemoryObject>> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

// zx_vmar_map
fit::result<zx_status_t, size_t> MemoryObject::map_in_vmar(const Vmar& vmar, size_t vmar_offset,
                                                           uint64_t* memory_offset, size_t len,
                                                           MappingFlags flags) const {
  return ktl::visit(MemoryObject::overloaded{
                        [&](const Vmo& vmo) -> fit::result<zx_status_t, size_t> {
                          zx_vaddr_t mapped_addr;
                          auto result =
                              vmar_map_common(flags.bits(), vmar.dispatcher(), vmar_offset,
                                              vmar.vmar->rights(), vmo.dispatcher()->vmo(),
                                              *memory_offset, vmo.vmo->rights(), len, &mapped_addr);

                          if (result != ZX_OK) {
                            return fit::error(result);
                          }
                          return fit::ok(mapped_addr);
                        },
                        [](const RingBuf&) -> fit::result<zx_status_t, size_t> {
                          return fit::error(ZX_ERR_NOT_SUPPORTED);
                        },
                    },
                    variant_);
}

}  // namespace starnix
