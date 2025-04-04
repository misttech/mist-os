// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_FZL_VMO_MAPPER_H_
#define LIB_FZL_VMO_MAPPER_H_

#include <lib/fzl/vmar-manager.h>

#include <utility>

#include <fbl/macros.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_object_paged.h>

namespace fzl {

class VmoMapper {
 public:
  VmoMapper() = default;
  ~VmoMapper() { Unmap(); }
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(VmoMapper);

  // Move support
  VmoMapper(VmoMapper&& other) { MoveFromOther(&other); }

  VmoMapper& operator=(VmoMapper&& other) {
    Unmap();
    MoveFromOther(&other);
    return *this;
  }

  // Create a new VMO and map it into our address space using the provided map
  // flags and optional target VMAR.  If requested, return the created VMO
  // with the requested rights.
  //
  // size         : The minimum size, in bytes, of the VMO to create.
  // map_flags    : The flags to use when mapping the VMO.
  // vmar         : A reference to a VmarManager to use when mapping the VMO, or
  //                nullptr to map the VMO using the root VMAR.
  // vmo_out      : A pointer which will receive the created VMO handle, or
  //                nullptr if the handle should be simply closed after it has
  //                been mapped.
  // vmo_rights   : The rights which should be applied to the VMO which is
  //                passed back to the user via vmo_out, or ZX_RIGHT_SAME_RIGHTS
  //                to leave the default rights.
  // cache_policy : When non-zero, indicates the cache policy to apply to the
  //                created VMO.
  // vmo_options  : The options to use when creating the VMO.
  zx_status_t CreateAndMap(uint64_t size,
                           zx_vm_option_t map_flags = ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                           fbl::RefPtr<VmarManager> vmar_manager = nullptr,
                           KernelHandle<VmObjectDispatcher>* vmo_out = nullptr,
                           zx_rights_t vmo_rights = ZX_RIGHT_SAME_RIGHTS, uint32_t cache_policy = 0,
                           uint32_t vmo_options = 0);

  // Map an existing VMO our address space using the provided map
  // flags and optional target VMAR.
  //
  // vmo        : The vmo to map.
  // offset     : The offset into the vmo, in bytes, to start the map
  // size       : The amount of the vmo, in bytes, to map, or 0 to map from
  //              the offset to the end of the VMO.
  // map_flags  : The flags to use when mapping the VMO.
  // vmar       : A reference to a VmarManager to use when mapping the VMO, or
  //              nullptr to map the VMO using the root VMAR.
  zx_status_t Map(const fbl::RefPtr<VmObjectDispatcher>& vmo, uint64_t offset = 0,
                  uint64_t size = 0, zx_vm_option_t map_flags = ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                  fbl::RefPtr<VmarManager> vmar_manager = nullptr);

  // Unmap the VMO from whichever VMAR it was mapped into.
  void Unmap();

  void* start() const { return reinterpret_cast<void*>(start_); }
  uint64_t size() const { return size_; }
  const fbl::RefPtr<VmarManager>& manager() const { return vmar_manager_; }

 protected:
  zx_status_t CheckReadyToMap(const fbl::RefPtr<VmarManager>& vmar_manager);
  zx_status_t InternalMap(const fbl::RefPtr<VmObjectDispatcher>& vmo, uint64_t offset,
                          uint64_t size, zx_vm_option_t map_flags,
                          fbl::RefPtr<VmarManager> vmar_manager);

  void MoveFromOther(VmoMapper* other) {
    vmar_manager_ = std::move(other->vmar_manager_);
    mapping_ = std::move(other->mapping_);

    start_ = other->start_;
    other->start_ = 0;

    size_ = other->size_;
    other->size_ = 0;
  }

  fbl::RefPtr<VmarManager> vmar_manager_;
  fbl::RefPtr<VmMapping> mapping_;
  uintptr_t start_ = 0;
  uint64_t size_ = 0;
};

}  // namespace fzl

#endif  // LIB_FZL_VMO_MAPPER_H_
