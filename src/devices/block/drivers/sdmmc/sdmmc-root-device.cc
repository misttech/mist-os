// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdmmc-root-device.h"

#include <inttypes.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/metadata.h>

#include <memory>

#include <fbl/alloc_checker.h>

#include "sdio-controller-device.h"
#include "sdmmc-block-device.h"

namespace sdmmc {

zx::result<> SdmmcRootDevice::Start() {
  parent_node_.Bind(std::move(node()));

  fidl::Arena arena;
  fidl::ObjectView<fuchsia_hardware_sdmmc::wire::SdmmcMetadata> sdmmc_metadata;
  {
    zx::result parsed_metadata = GetMetadata(arena);
    if (parsed_metadata.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to GetMetadata: %s",
               zx_status_get_string(parsed_metadata.error_value()));
      return parsed_metadata.take_error();
    }
    sdmmc_metadata = *parsed_metadata;
  }

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to create controller endpoints: %s",
             controller_endpoints.status_string());
    return controller_endpoints.take_error();
  }

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (!node_endpoints.is_ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to create node endpoints: %s",
             node_endpoints.status_string());
    return node_endpoints.take_error();
  }

  controller_.Bind(std::move(controller_endpoints->client));
  root_node_.Bind(std::move(node_endpoints->client));

  const auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, name()).Build();

  auto result = parent_node_->AddChild(args, std::move(controller_endpoints->server),
                                       std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to add child: %s", result.status_string());
    return zx::error(result.status());
  }

  zx_status_t status = Init(sdmmc_metadata);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok();
}

// Returns nullptr if device was successfully added. Returns the SdmmcDevice if the probe failed
// (i.e., no eligible device present).
template <class DeviceType>
zx::result<std::unique_ptr<SdmmcDevice>> SdmmcRootDevice::MaybeAddDevice(
    const std::string& name, std::unique_ptr<SdmmcDevice> sdmmc,
    const fuchsia_hardware_sdmmc::wire::SdmmcMetadata& metadata) {
  if (zx_status_t st = sdmmc->Init(metadata.use_fidl()) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to initialize SdmmcDevice: %s", zx_status_get_string(st));
    return zx::error(st);
  }

  std::unique_ptr<DeviceType> device;
  if (zx_status_t st = DeviceType::Create(this, std::move(sdmmc), &device) != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to create %s device: %s", name.c_str(),
             zx_status_get_string(st));
    return zx::error(st);
  }

  if (zx_status_t st = device->Probe(metadata); st != ZX_OK) {
    return zx::ok(std::move(device->TakeSdmmcDevice()));
  }

  if (zx_status_t st = device->AddDevice(); st != ZX_OK) {
    return zx::error(st);
  }

  child_device_ = std::move(device);
  return zx::ok(nullptr);
}

zx::result<fidl::ObjectView<fuchsia_hardware_sdmmc::wire::SdmmcMetadata>>
SdmmcRootDevice::GetMetadata(fidl::AnyArena& arena) {
  zx::result decoded = compat::GetMetadata<fuchsia_hardware_sdmmc::wire::SdmmcMetadata>(
      incoming(), arena, DEVICE_METADATA_SDMMC);

  constexpr uint32_t kMaxCommandPacking = 16;

  if (decoded.is_error()) {
    if (decoded.status_value() == ZX_ERR_NOT_FOUND) {
      FDF_LOGL(INFO, logger(), "No metadata provided");
      return zx::ok(
          fidl::ObjectView(arena, fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(arena)
                                      .enable_trim(true)
                                      .enable_cache(true)
                                      .removable(false)
                                      .max_command_packing(kMaxCommandPacking)
                                      .use_fidl(true)
                                      .Build()));
    }
    FDF_LOGL(ERROR, logger(), "Failed to decode metadata: %s", decoded.status_string());
    return decoded.take_error();
  }

  // Default to trim and cache enabled, non-removable.
  return zx::ok(fidl::ObjectView(
      arena,
      fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(arena)
          .enable_trim(!decoded->has_enable_trim() || decoded->enable_trim())
          .enable_cache(!decoded->has_enable_cache() || decoded->enable_cache())
          .removable(decoded->has_removable() && decoded->removable())
          .max_command_packing(decoded->has_max_command_packing() ? decoded->max_command_packing()
                                                                  : kMaxCommandPacking)
          .use_fidl(!decoded->has_use_fidl() || decoded->use_fidl())
          .Build()));
}

zx_status_t SdmmcRootDevice::Init(
    fidl::ObjectView<fuchsia_hardware_sdmmc::wire::SdmmcMetadata> metadata) {
  auto sdmmc = std::make_unique<SdmmcDevice>(this);

  // Probe for SDIO first, then SD/MMC.
  zx::result<std::unique_ptr<SdmmcDevice>> result =
      MaybeAddDevice<SdioControllerDevice>("sdio", std::move(sdmmc), *metadata);
  if (result.is_error()) {
    return result.status_value();
  } else if (*result == nullptr) {
    return ZX_OK;
  }
  result = MaybeAddDevice<SdmmcBlockDevice>("block", std::move(*result), *metadata);
  if (result.is_error()) {
    return result.status_value();
  } else if (*result == nullptr) {
    return ZX_OK;
  }

  if (metadata->removable()) {
    // This controller is connected to a removable card slot, and no card was inserted. Indicate
    // success so that our device remains available.
    // TODO(https://fxbug.dev/130283): Enable detection of card insert/removal after initialization.
    FDF_LOGL(INFO, logger(), "failed to probe removable device");
    return ZX_OK;
  }

  // Failure to probe a hardwired device is unexpected. Reply with an error code so that our device
  // gets removed.
  FDF_LOGL(ERROR, logger(), "failed to probe irremovable device");
  return ZX_ERR_NOT_FOUND;
}

}  // namespace sdmmc
