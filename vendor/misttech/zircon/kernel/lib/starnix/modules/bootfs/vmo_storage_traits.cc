// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/modules/bootfs/vmo_storage_traits.h"

#include <lib/fit/result.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <vm/vm_object_paged.h>

namespace zbitl {

namespace {

fit::result<zx_status_t, bool> IsResizable(const fbl::RefPtr<VmObject>& vmo) {
  return fit::ok(vmo->is_resizable());
}

}  // namespace

fit::result<zx_status_t, uint32_t> StorageTraits<fbl::RefPtr<VmObject>>::Capacity(
    const fbl::RefPtr<VmObject>& vmo) {
  uint64_t size = vmo->size();
  return fit::ok(static_cast<uint32_t>(
      std::min(static_cast<uint64_t>(std::numeric_limits<uint32_t>::max()), size)));
}

fit::result<zx_status_t> StorageTraits<fbl::RefPtr<VmObject>>::EnsureCapacity(
    const fbl::RefPtr<VmObject>& vmo, uint32_t capacity_bytes) {
  auto current = Capacity(vmo);
  if (current.is_error()) {
    return current.take_error();
  } else if (current.value() >= capacity_bytes) {
    return fit::ok();  // Current capacity is sufficient.
  }

  uint64_t cap = static_cast<uint64_t>(capacity_bytes);
  if (auto status = vmo->Resize(cap); status != ZX_OK) {
    return fit::error{status};
  }
  return fit::ok();
}

fit::result<zx_status_t> StorageTraits<fbl::RefPtr<VmObject>>::Read(
    const fbl::RefPtr<VmObject>& vmo, payload_type payload, void* buffer, uint32_t length) {
  zx_status_t status = vmo->Read(buffer, payload, length);
  if (status != ZX_OK) {
    return fit::error{status};
  }
  return fit::ok();
}

fit::result<zx_status_t> StorageTraits<fbl::RefPtr<VmObject>>::Write(
    const fbl::RefPtr<VmObject>& vmo, uint32_t offset, ByteView data) {
  zx_status_t status = vmo->Write(data.data(), offset, data.size());
  if (status != ZX_OK) {
    return fit::error{status};
  }
  return fit::ok();
}

fit::result<zx_status_t, fbl::RefPtr<VmObject>> StorageTraits<fbl::RefPtr<VmObject>>::Create(
    const fbl::RefPtr<VmObject>& old, uint32_t size, uint32_t initial_zero_size) {
  // While `initial_zero_size` is a required parameter for the creation trait,
  // it is unnecessary in the case of VMOs, as newly-created instances are
  // always zero-filled.

  // Make the new VMO resizable only if the original is.
  uint32_t options = 0;
  if (auto result = IsResizable(old); result.is_error()) {
    return result.take_error();
  } else if (result.value()) {
    options |= VmObjectPaged::kResizable;
  }

  fbl::RefPtr<VmObjectPaged> vmo;
  uint64_t aligned_size;
  zx_status_t status = VmObject::RoundSize(size, &aligned_size);
  if (status != ZX_OK) {
    return fit::error(status);
  }
  status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, options, aligned_size, &vmo);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  return fit::ok(std::move(vmo));
}

fit::result<zx_status_t> StorageTraits<fbl::RefPtr<VmObject>>::DoRead(
    const fbl::RefPtr<VmObject>& vmo, uint64_t offset, uint32_t length, bool (*cb)(void*, ByteView),
    void* arg) {
  if (length == 0) {
    cb(arg, {});
    return fit::ok();
  }

  // This always copies, when mapping might be better for large sizes.  But
  // address space is cheap, so users concerned with large sizes should just
  // map the whole ZBI in and use View<std::span> instead.
  auto size = [&]() { return std::min(static_cast<uint32_t>(kBufferedReadChunkSize), length); };
  fbl::AllocChecker ac;
  std::unique_ptr<std::byte[]> buf{new (&ac) std::byte[size()]};
  if (!ac.check()) {
    return fit::error(ZX_ERR_NO_MEMORY);
  }

  while (length > 0) {
    const uint32_t n = size();
    zx_status_t status = vmo->Read(buf.get(), offset, n);
    if (status != ZX_OK) {
      return fit::error{status};
    }
    if (!cb(arg, {buf.get(), n})) {
      break;
    }
    offset += n;
    length -= n;
  }

  return fit::ok();
}

fit::result<zx_status_t, std::optional<std::pair<fbl::RefPtr<VmObject>, uint32_t>>>
StorageTraits<fbl::RefPtr<VmObject>>::DoClone(const fbl::RefPtr<VmObject>& original,
                                              uint32_t offset, uint32_t length) {
  const uint32_t slop = offset % uint32_t{ZX_PAGE_SIZE};
  const uint32_t clone_start = offset & -uint32_t{ZX_PAGE_SIZE};
  const uint32_t clone_size = slop + length;

  // Make the child resizable only if the parent is.
  CloneType type = CloneType::Snapshot;
  Resizability resizable = Resizability::NonResizable;
  if (auto result = IsResizable(original); result.is_error()) {
    return result.take_error();
  } else if (result.value()) {
    resizable = Resizability::Resizable;
  }

  fbl::RefPtr<VmObjectPaged> vmo;
  uint64_t aligned_size;
  zx_status_t status = VmObject::RoundSize(clone_size, &aligned_size);
  if (status != ZX_OK) {
    return fit::error(status);
  }

  fbl::RefPtr<VmObject> child_vmo;
  status = original->CreateClone(resizable, type, clone_start, aligned_size, true, &child_vmo);
  if (status == ZX_OK && slop > 0) {
    // Explicitly zero the partial page before the range so it remains unseen.
    status = child_vmo->ZeroRange(offset, slop);
  }
  if (status == ZX_OK && clone_size % ZX_PAGE_SIZE != 0) {
    // Explicitly zero the partial page after the range so it remains unseen.
    status = child_vmo->ZeroRange(clone_size, ZX_PAGE_SIZE - (clone_size % ZX_PAGE_SIZE));
  }
  if (status != ZX_OK) {
    return fit::error{status};
  }

  return fit::ok(std::make_pair(std::move(child_vmo), slop));
}

}  // namespace zbitl
