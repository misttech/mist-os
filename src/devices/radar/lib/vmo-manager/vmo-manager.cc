// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vmo-manager.h"

#include <fbl/auto_lock.h>

namespace radar {

using StatusCode = fuchsia_hardware_radar::StatusCode;

VmoManager::VmoManager(const size_t minimum_vmo_size)
    : minimum_vmo_size_(minimum_vmo_size),
      registered_vmos_(vmo_store::Options{
          .map = {{
              .vm_option = ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
              .vmar = nullptr,
          }},
          .pin = {},
      }) {}

VmoManager::~VmoManager() {
  // These must be manually cleared before destruction to avoid triggering an assert.
  locked_vmos_.clear();
  unlocked_vmos_.clear();
}

fit::result<StatusCode, uint32_t> VmoManager::WriteUnlockedVmoAndGetId(
    const cpp20::span<const uint8_t> data) {
  fbl::AutoLock lock(&lock_);

  // Take a VMO from the front of the unlocked list and move it to the back of the locked list.
  VmoMeta* vmo = unlocked_vmos_.pop_front();
  if (vmo == nullptr) {
    return fit::error(StatusCode::kOutOfVmos);
  }

  if (data.size_bytes() > vmo->vmo_data.size_bytes()) {
    return fit::error(StatusCode::kVmoTooSmall);
  }

  locked_vmos_.push_back(vmo);
  memcpy(vmo->vmo_data.data(), data.data(), data.size_bytes());
  return fit::success(vmo->vmo_id);
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

StatusCode VmoManager::RegisterVmos(fidl::VectorView<const uint32_t> vmo_ids,
                                    fidl::VectorView<zx::vmo> vmos) {
  if (vmo_ids.count() != vmos.count() ||
      vmo_ids.count() > fuchsia_hardware_radar::wire::kVmoVectorMaxCount) {
    return StatusCode::kInvalidArgs;
  }

  fbl::AutoLock lock(&lock_);

  zx_status_t status = ZX_OK;
  size_t last_vmo_index = 0;
  for (; last_vmo_index < vmo_ids.count(); last_vmo_index++) {
    const uint32_t vmo_id = vmo_ids[last_vmo_index];

    status = registered_vmos_.RegisterWithKey(
        vmo_id, vmo_store::StoredVmo<VmoMeta>(std::move(vmos[last_vmo_index]), {}));
    if (status != ZX_OK) {
      break;
    }

    vmo_store::StoredVmo<VmoMeta>* vmo = registered_vmos_.GetVmo(vmo_id);
    ZX_ASSERT(vmo != nullptr);
    if (vmo->data().size_bytes() < minimum_vmo_size_) {
      status = ZX_ERR_BUFFER_TOO_SMALL;
      break;
    }

    vmo->meta().vmo_id = vmo_id;
    vmo->meta().vmo_data = vmo->data().subspan(0, minimum_vmo_size_);
    unlocked_vmos_.push_back(&vmo->meta());
  }

  // Registration for one of the VMOs failed, so undo any previous registrations that may have
  // succeeded.
  if (status != ZX_OK) {
    for (size_t i = 0; i <= last_vmo_index; i++) {
      const uint32_t vmo_id = vmo_ids[i];
      // Only unregister the final VMO if the status code isn't ZX_ERR_ALREADY_EXISTS. Otherwise
      // this would unregister a VMO that had been registered in a previous call call.
      if (i != last_vmo_index || status != ZX_ERR_ALREADY_EXISTS) {
        unlocked_vmos_.erase_if([vmo_id](const VmoMeta& vmo) { return vmo.vmo_id == vmo_id; });

        // We can't return handles to the caller, so just close them.
        zx::result<zx::vmo> vmo = registered_vmos_.Unregister(vmo_id);
      }
    }
  }

  switch (status) {
    case ZX_OK:
      return StatusCode::kSuccess;
    case ZX_ERR_BAD_HANDLE:
    case ZX_ERR_WRONG_TYPE:
    case ZX_ERR_BAD_STATE:
      return StatusCode::kVmoBadHandle;
    case ZX_ERR_ALREADY_EXISTS:
      return StatusCode::kVmoAlreadyRegistered;
    case ZX_ERR_ACCESS_DENIED:
      return StatusCode::kVmoAccessDenied;
    case ZX_ERR_OUT_OF_RANGE:
    case ZX_ERR_BUFFER_TOO_SMALL:
      return StatusCode::kVmoTooSmall;
    default:
      return StatusCode::kUnspecified;
  }
}

StatusCode VmoManager::RegisterVmos(fidl::VectorView<uint32_t> vmo_ids,
                                    fidl::VectorView<zx::vmo> vmos) {
  return RegisterVmos(
      fidl::VectorView<const uint32_t>::FromExternal(vmo_ids.data(), vmo_ids.count()), vmos);
}

fit::result<StatusCode> VmoManager::RegisterVmos(const std::vector<uint32_t>& vmo_ids,
                                                 std::vector<zx::vmo> vmos) {
  const StatusCode status =
      RegisterVmos(fidl::VectorView<const uint32_t>::FromExternal(vmo_ids.data(), vmo_ids.size()),
                   fidl::VectorView<zx::vmo>::FromExternal(vmos.data(), vmos.size()));
  if (status == StatusCode::kSuccess) {
    return fit::success();
  }
  return fit::error(status);
}

StatusCode VmoManager::UnregisterVmos(fidl::VectorView<const uint32_t> vmo_ids,
                                      fidl::VectorView<zx::vmo> out_vmos) {
  if (vmo_ids.count() != out_vmos.count() ||
      vmo_ids.count() > fuchsia_hardware_radar::wire::kVmoVectorMaxCount) {
    return StatusCode::kInvalidArgs;
  }

  fbl::AutoLock lock(&lock_);

  for (const uint32_t vmo_id : vmo_ids) {
    vmo_store::StoredVmo<VmoMeta>* vmo = registered_vmos_.GetVmo(vmo_id);
    if (vmo == nullptr) {
      return StatusCode::kVmoNotFound;
    }
  }

  for (size_t i = 0; i < vmo_ids.count(); i++) {
    const uint32_t vmo_id = vmo_ids[i];

    unlocked_vmos_.erase_if([vmo_id](const VmoMeta& vmo) { return vmo.vmo_id == vmo_id; });
    locked_vmos_.erase_if([vmo_id](const VmoMeta& vmo) { return vmo.vmo_id == vmo_id; });

    auto status = registered_vmos_.Unregister(vmo_id);
    ZX_ASSERT(status.is_ok());
    out_vmos[i] = std::move(status.value());
  }

  return StatusCode::kSuccess;
}

StatusCode VmoManager::UnregisterVmos(fidl::VectorView<uint32_t> vmo_ids,
                                      fidl::VectorView<zx::vmo> out_vmos) {
  return UnregisterVmos(
      fidl::VectorView<const uint32_t>::FromExternal(vmo_ids.data(), vmo_ids.count()), out_vmos);
}

fit::result<StatusCode, std::vector<zx::vmo>> VmoManager::UnregisterVmos(
    const std::vector<uint32_t>& vmo_ids) {
  std::vector<zx::vmo> vmos;
  vmos.resize({});

  const StatusCode status =
      UnregisterVmos(fidl::VectorView<const uint32_t>::FromExternal(vmo_ids.data(), vmo_ids.size()),
                     fidl::VectorView<zx::vmo>::FromExternal(vmos.data(), vmos.size()));
  if (status == StatusCode::kSuccess) {
    return fit::success(std::move(vmos));
  }
  return fit::error(status);
}

}  // namespace radar
