// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vmo-manager.h"

#include <fbl/auto_lock.h>

namespace radar {

using StatusCode = fuchsia_hardware_radar::wire::StatusCode;

std::optional<VmoManager::RegisteredVmo> VmoManager::GetUnlockedVmo() {
  fbl::AutoLock lock(&lock_);

  // Take a VMO from the front of the unlocked list and move it to the back of the locked list.
  VmoMeta* vmo = unlocked_vmos_.pop_front();
  if (vmo == nullptr) {
    return std::optional<RegisteredVmo>();
  }

  locked_vmos_.push_back(vmo);
  return std::optional<RegisteredVmo>({.vmo_id = vmo->vmo_id, .vmo_data = vmo->vmo_data});
}

void VmoManager::UnlockVmo(uint32_t vmo_id) {
  fbl::AutoLock lock(&lock_);

  // Find the VMO and move it to the back of the unlocked list. If the client is unlocking VMOs in
  // the order that they were received then this VMO should be the first in the locked list.
  const auto it =
      locked_vmos_.find_if([vmo_id](const VmoMeta& vmo) { return vmo.vmo_id == vmo_id; });
  if (it.IsValid()) {
    VmoMeta* vmo = locked_vmos_.erase(it);
    unlocked_vmos_.push_back(vmo);
  }
}

StatusCode VmoManager::RegisterVmos(fidl::VectorView<uint32_t> vmo_ids,
                                    fidl::VectorView<zx::vmo> vmos) {
  if (vmo_ids.count() != vmos.count()) {
    return StatusCode::kInvalidArgs;
  }

  fbl::AutoLock lock(&lock_);

  for (size_t i = 0; i < vmo_ids.count(); i++) {
    if (registered_vmos_.GetVmo(vmo_ids[i]) != nullptr) {
      return StatusCode::kVmoAlreadyRegistered;
    }

    zx_status_t status = registered_vmos_.RegisterWithKey(
        vmo_ids[i], vmo_store::StoredVmo<VmoMeta>(std::move(vmos[i]), {}));
    if (status == ZX_ERR_BAD_HANDLE) {
      return StatusCode::kVmoBadHandle;
    }
    if (status == ZX_ERR_ACCESS_DENIED) {
      return StatusCode::kVmoAccessDenied;
    }
    if (status == ZX_ERR_BUFFER_TOO_SMALL) {
      return StatusCode::kVmoTooSmall;
    }
    if (status != ZX_OK) {
      return StatusCode::kUnspecified;
    }

    vmo_store::StoredVmo<VmoMeta>* vmo = registered_vmos_.GetVmo(vmo_ids[i]);
    ZX_ASSERT(vmo != nullptr);
    if (vmo->data().size_bytes() < minimum_vmo_size_) {
      return StatusCode::kVmoTooSmall;
    }

    vmo->meta().vmo_id = vmo_ids[i];
    vmo->meta().vmo_data = vmo->data();
    unlocked_vmos_.push_back(&vmo->meta());
  }

  return StatusCode::kSuccess;
}

StatusCode VmoManager::UnregisterVmos(fidl::VectorView<uint32_t> vmo_ids,
                                      fidl::VectorView<zx::vmo> out_vmos) {
  fbl::AutoLock lock(&lock_);

  if (vmo_ids.count() > fuchsia_hardware_radar::wire::kVmoVectorMaxCount) {
    return StatusCode::kInvalidArgs;
  }

  for (size_t i = 0; i < vmo_ids.count(); i++) {
    const uint32_t vmo_id = vmo_ids[i];

    const auto unlocked_it =
        unlocked_vmos_.find_if([vmo_id](const VmoMeta& vmo) { return vmo.vmo_id == vmo_id; });
    unlocked_vmos_.erase(unlocked_it);

    const auto locked_it =
        locked_vmos_.find_if([vmo_id](const VmoMeta& vmo) { return vmo.vmo_id == vmo_id; });
    locked_vmos_.erase(locked_it);

    auto status = registered_vmos_.Unregister(vmo_id);
    if (status.is_error()) {
      return StatusCode::kVmoNotFound;
    }

    out_vmos[i] = std::move(status.value());
  }

  return StatusCode::kSuccess;
}

}  // namespace radar
