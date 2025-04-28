// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mmio/mmio-buffer.h>
#include <zircon/errors.h>
// #include <zircon/process.h>
// #include <zircon/syscalls.h>
// #include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <fbl/ref_ptr.h>
#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_object_paged.h>
#include <vm/vm_object_physical.h>

#define MMIO_ROUNDUP(a, b)      \
  ({                            \
    const __typeof(a) _a = (a); \
    const __typeof(b) _b = (b); \
    ((_a + _b - 1) / _b * _b);  \
  })
#define MMIO_ROUNDDOWN(a, b)    \
  ({                            \
    const __typeof(a) _a = (a); \
    const __typeof(b) _b = (b); \
    _a - (_a % _b);             \
  })

zx_status_t mmio_buffer_init(mmio_buffer_t* buffer, zx_off_t offset, size_t size, void* vmo,
                             uint32_t cache_policy) {
  ZX_DEBUG_ASSERT(vmo);
  if (!buffer) {
    // zx_handle_close(vmo);
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::RefPtr<VmObjectDispatcher> vmo_dispatcher = fbl::ImportFromRawPtr((VmObjectDispatcher*)vmo);

  // |zx_vmo_set_cache_policy| will always return an error if it encounters a
  // VMO that has already been mapped. To enable tests where a VMO may be mapped
  // and modified already by a test fixture we only set the cache policy of a
  // provided VMO if the requested cache policy does not match the VMO's current
  // cache policy.
  zx_info_vmo_t info = vmo_dispatcher->GetVmoInfo(ZX_RIGHT_READ | ZX_RIGHT_WRITE);

  if (info.cache_policy != cache_policy) {
    zx_status_t status = vmo_dispatcher->SetMappingCachePolicy(cache_policy);
    // status = zx_vmo_set_cache_policy(vmo, cache_policy);
    if (status != ZX_OK) {
      //  zx_handle_close(vmo);
      return status;
    }
  }

  uint64_t result = 0;
  if (add_overflow(offset, size, &result) || result > info.size_bytes) {
    // zx_handle_close(vmo);
    return ZX_ERR_OUT_OF_RANGE;
  }

  const size_t vmo_offset = MMIO_ROUNDDOWN(offset, PAGE_SIZE);
  const size_t page_offset = offset - vmo_offset;
  const size_t vmo_size = MMIO_ROUNDUP(size + page_offset, PAGE_SIZE);

  zx::result<VmAddressRegion::MapResult> mapping_result =
      VmAspace::kernel_aspace()->RootVmar()->CreateVmMapping(
          0, vmo_size, 0, 0 /* vmar_flags */, vmo_dispatcher->vmo(), vmo_offset,
          (ARCH_MMU_FLAG_CACHED & cache_policy) | ARCH_MMU_FLAG_PERM_READ |
              ARCH_MMU_FLAG_PERM_WRITE,
          "mmio-buffer");
  if (mapping_result.is_error()) {
    return mapping_result.error_value();
  }

  // Setup a handler to destroy the new mapping if the syscall is unsuccessful.
  auto cleanup_handler = fit::defer([&mapping_result]() { mapping_result->mapping->Destroy(); });

  // Prepopulate the mapping's page tables so there are no page faults taken.
  zx_status_t status = mapping_result->mapping->MapRange(0, size, true);
  if (status != ZX_OK) {
    return status;
  }

  cleanup_handler.cancel();

  buffer->mapping = (void*)fbl::ExportToRawPtr(&mapping_result->mapping);
  buffer->vmo = vmo;
  buffer->vaddr = (MMIO_PTR void*)(mapping_result->base + page_offset);
  buffer->offset = offset;
  buffer->size = size;

  return ZX_OK;
}

zx_status_t mmio_buffer_init_physical(mmio_buffer_t* buffer, zx_paddr_t base, size_t size,
                                      zx_handle_t resource, uint32_t cache_policy) {
  fbl::RefPtr<VmObjectPhysical> vmo;
  zx_status_t status = VmObjectPhysical::Create(base, size, &vmo);
  if (status != ZX_OK) {
    return status;
  }

  // |base| is guaranteed to be page aligned.
  return mmio_buffer_init(buffer, 0, size, fbl::ExportToRawPtr(&vmo), cache_policy);
}

void mmio_buffer_release(mmio_buffer_t* buffer) {
  if (buffer->mapping != nullptr) {
    fbl::RefPtr<VmMapping> mapping = fbl::ImportFromRawPtr((VmMapping*)buffer->mapping);
    mapping->Destroy();
    buffer->mapping = nullptr;
    buffer->vmo = nullptr;
  }
}

zx_status_t mmio_buffer_pin(mmio_buffer_t* buffer, void* bti, mmio_pinned_buffer_t* out) {
#if 0
  zx_paddr_t paddr;
  // zx_handle_t pmt;
  const uint32_t options = ZX_BTI_PERM_WRITE | ZX_BTI_PERM_READ | ZX_BTI_CONTIGUOUS;
  const size_t vmo_offset = MMIO_ROUNDDOWN(buffer->offset, PAGE_SIZE);
  const size_t page_offset = buffer->offset - vmo_offset;
  const size_t vmo_size = MMIO_ROUNDUP(buffer->size + page_offset, PAGE_SIZE);

  // zx_status_t status = zx_bti_pin(bti, options, buffer->vmo, vmo_offset, vmo_size, &paddr, 1,
  // &pmt);
  //if (status != ZX_OK) {
  //  return status;
  //}

  out->mmio = buffer;
  out->paddr = paddr + page_offset;
  out->pmt = pmt;
#endif
  return ZX_ERR_NOT_SUPPORTED;
}

void mmio_buffer_unpin(mmio_pinned_buffer_t* buffer) {
  if (buffer->pmt != nullptr) {
    // zx_pmt_unpin(buffer->pmt);
    buffer->pmt = nullptr;
  }
}
