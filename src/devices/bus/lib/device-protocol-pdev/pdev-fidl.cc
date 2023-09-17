// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/debug.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>

#include <ddktl/device.h>

namespace ddk {

PDevFidl::PDevFidl(zx_device_t* parent) {
  zx::result client =
      ddk::Device<void>::DdkConnectFidlProtocol<fuchsia_hardware_platform_device::Service::Device>(
          parent);
  if (client.is_error()) {
    return;
  }
  pdev_ = fidl::WireSyncClient(std::move(client.value()));
}

PDevFidl::PDevFidl(zx_device_t* parent, const char* fragment_name) {
  zx::result client = ddk::Device<void>::DdkConnectFragmentFidlProtocol<
      fuchsia_hardware_platform_device::Service::Device>(parent, fragment_name);
  if (client.is_error()) {
    return;
  }
  pdev_ = fidl::WireSyncClient(std::move(client.value()));
}

PDevFidl::PDevFidl(fidl::ClientEnd<fuchsia_hardware_platform_device::Device> client)
    : pdev_(std::move(client)) {}

zx::result<PDevFidl> PDevFidl::Create(zx_device_t* parent) {
  zx::result client =
      ddk::Device<void>::DdkConnectFidlProtocol<fuchsia_hardware_platform_device::Service::Device>(
          parent);
  if (client.is_error()) {
    return client.take_error();
  }
  return zx::ok(PDevFidl(std::move(client.value())));
}

zx::result<PDevFidl> PDevFidl::Create(zx_device_t* parent, const char* fragment_name) {
  zx::result client = ddk::Device<void>::DdkConnectFragmentFidlProtocol<
      fuchsia_hardware_platform_device::Service::Device>(parent, fragment_name);
  if (client.is_error()) {
    return client.take_error();
  }
  return zx::ok(PDevFidl(std::move(client.value())));
}

PDevFidl PDevFidl::FromFragment(zx_device_t* parent) { return PDevFidl(parent, kFragmentName); }

zx_status_t PDevFidl::FromFragment(zx_device_t* parent, PDevFidl* out) {
  *out = PDevFidl(parent, kFragmentName);
  if (!out->is_valid()) {
    return ZX_ERR_NO_RESOURCES;
  }
  return ZX_OK;
}

void PDevFidl::ShowInfo() {
  pdev_device_info_t info;
  if (GetDeviceInfo(&info) == ZX_OK) {
    zxlogf(INFO, "VID:PID:DID         = %04x:%04x:%04x", info.vid, info.pid, info.did);
    zxlogf(INFO, "mmio count          = %d", info.mmio_count);
    zxlogf(INFO, "irq count           = %d", info.irq_count);
    zxlogf(INFO, "bti count           = %d", info.bti_count);
  }
}

zx_status_t PDevFidl::MapMmio(uint32_t index, std::optional<fdf::MmioBuffer>* mmio,
                              uint32_t cache_policy) {
  pdev_mmio_t pdev_mmio = {};

  zx_status_t status = GetMmio(index, &pdev_mmio);
  if (status != ZX_OK) {
    return status;
  }
  return PDevMakeMmioBufferWeak(pdev_mmio, mmio, cache_policy);
}

zx_status_t PDevFidl::GetMmio(uint32_t index, pdev_mmio_t* out_mmio) const {
  fidl::WireResult result = pdev_->GetMmio(index);
  if (result.status() != ZX_OK) {
    return result.status();
  }
  if (!result->is_ok()) {
    return result->error_value();
  }
  if (result->value()->has_offset()) {
    out_mmio->offset = result->value()->offset();
  }
  if (result->value()->has_size()) {
    out_mmio->size = result->value()->size();
  }
  if (result->value()->has_vmo()) {
    out_mmio->vmo = result->value()->vmo().release();
  }
  return ZX_OK;
}

zx_status_t PDevFidl::GetInterrupt(uint32_t index, uint32_t flags, zx::interrupt* out_irq) {
  fidl::WireResult result = pdev_->GetInterrupt(index, flags);
  if (result.status() != ZX_OK) {
    return result.status();
  }
  if (!result->is_ok()) {
    return result->error_value();
  }
  *out_irq = std::move(result->value()->irq);
  return ZX_OK;
}

zx_status_t PDevFidl::GetBti(uint32_t index, zx::bti* out_bti) {
  fidl::WireResult result = pdev_->GetBti(index);
  if (result.status() != ZX_OK) {
    return result.status();
  }
  if (!result->is_ok()) {
    return result->error_value();
  }
  *out_bti = std::move(result->value()->bti);
  return ZX_OK;
}

zx_status_t PDevFidl::GetSmc(uint32_t index, zx::resource* out_smc) const {
  fidl::WireResult result = pdev_->GetSmc(index);
  if (result.status() != ZX_OK) {
    return result.status();
  }
  if (!result->is_ok()) {
    return result->error_value();
  }
  *out_smc = std::move(result->value()->smc);
  return ZX_OK;
}

zx_status_t PDevFidl::GetDeviceInfo(pdev_device_info_t* out_info) {
  fidl::WireResult result = pdev_->GetDeviceInfo();
  if (result.status() != ZX_OK) {
    return result.status();
  }
  if (!result->is_ok()) {
    return result->error_value();
  }

  if (result->value()->has_vid()) {
    out_info->vid = result->value()->vid();
  }
  if (result->value()->has_pid()) {
    out_info->pid = result->value()->pid();
  }
  if (result->value()->has_did()) {
    out_info->did = result->value()->did();
  }
  if (result->value()->has_mmio_count()) {
    out_info->mmio_count = result->value()->mmio_count();
  }
  if (result->value()->has_irq_count()) {
    out_info->irq_count = result->value()->irq_count();
  }
  if (result->value()->has_bti_count()) {
    out_info->bti_count = result->value()->bti_count();
  }
  if (result->value()->has_smc_count()) {
    out_info->smc_count = result->value()->smc_count();
  }
  if (result->value()->has_metadata_count()) {
    out_info->metadata_count = result->value()->metadata_count();
  }
  if (result->value()->has_name()) {
    std::string name = std::string(result->value()->name().get());
    if (name.size() > sizeof(out_info->name)) {
      return ZX_ERR_BUFFER_TOO_SMALL;
    }
    strncpy(out_info->name, name.c_str(), sizeof(out_info->name));
  }

  return ZX_OK;
}

zx_status_t PDevFidl::GetBoardInfo(pdev_board_info_t* out_info) const {
  fidl::WireResult result = pdev_->GetBoardInfo();
  if (result.status() != ZX_OK) {
    return result.status();
  }
  if (!result->is_ok()) {
    return result->error_value();
  }

  if (result->value()->has_vid()) {
    out_info->vid = result->value()->vid();
  }
  if (result->value()->has_pid()) {
    out_info->pid = result->value()->pid();
  }
  if (result->value()->has_board_name()) {
    std::string board_name = std::string(result->value()->board_name().get());
    if (board_name.size() > sizeof(out_info->board_name)) {
      return ZX_ERR_BUFFER_TOO_SMALL;
    }
    strncpy(out_info->board_name, board_name.c_str(), sizeof(out_info->board_name));
  }
  if (result->value()->has_board_revision()) {
    out_info->board_revision = result->value()->board_revision();
  }
  return ZX_OK;
}

}  // namespace ddk
