// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_IO_BUFFER_SHARED_REGION_DISPATCHER_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_IO_BUFFER_SHARED_REGION_DISPATCHER_H_

#include <zircon/rights.h>
#include <zircon/types.h>

#include <ktl/byte.h>
#include <object/dispatcher.h>
#include <vm/vm_address_region.h>
#include <vm/vm_object.h>

class IoBufferSharedRegionDispatcher
    : public SoloDispatcher<IoBufferSharedRegionDispatcher, ZX_DEFAULT_IOB_SHARED_REGION_RIGHTS> {
 public:
  static zx_status_t Create(uint64_t size, KernelHandle<IoBufferSharedRegionDispatcher>* handle,
                            zx_rights_t* rights);

  ~IoBufferSharedRegionDispatcher() { mapping_->Destroy(); }

  // SoloDispatcher implementation.
  zx_obj_type_t get_type() const final { return ZX_OBJ_TYPE_IOB_SHARED_REGION; }

  zx::result<> Write(uint64_t tag, user_in_iovec_t message);

  const fbl::RefPtr<VmObjectPaged>& vmo() const { return vmo_; }

 private:
  // The header used with the mediated write ring buffer discipline.
  struct Header {
    ktl::atomic<uint64_t> head;
    ktl::atomic<uint64_t> tail;
  };

  IoBufferSharedRegionDispatcher(fbl::RefPtr<VmObjectPaged> vmo, fbl::RefPtr<VmMapping> mapping,
                                 zx_vaddr_t base)
      : vmo_(ktl::move(vmo)), mapping_(ktl::move(mapping)), base_(base) {
    vmo_->set_user_id(get_koid());
  }

  Header* GetHeader() const { return reinterpret_cast<Header*>(base_); }

  fbl::RefPtr<VmObjectPaged> vmo_;
  fbl::RefPtr<VmMapping> mapping_;
  zx_vaddr_t base_;
};

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_IO_BUFFER_SHARED_REGION_DISPATCHER_H_
