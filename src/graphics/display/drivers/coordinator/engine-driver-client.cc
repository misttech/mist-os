// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/engine-driver-client.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/driver/compat/cpp/banjo_client.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <cstdint>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"

namespace display {

namespace {

static constexpr fdf_arena_tag_t kArenaTag = 'DISP';

zx::result<std::unique_ptr<EngineDriverClient>> CreateFidlEngineDriverClient(
    fdf::Namespace& incoming) {
  zx::result<fdf::ClientEnd<fuchsia_hardware_display_engine::Engine>> connect_engine_client_result =
      incoming.Connect<fuchsia_hardware_display_engine::Service::Engine>();
  if (connect_engine_client_result.is_error()) {
    FDF_LOG(WARNING, "Failed to connect to display engine FIDL client: %s",
            connect_engine_client_result.status_string());
    return connect_engine_client_result.take_error();
  }
  fdf::ClientEnd<fuchsia_hardware_display_engine::Engine> engine_client =
      std::move(connect_engine_client_result).value();

  if (!engine_client.is_valid()) {
    FDF_LOG(WARNING, "Display engine FIDL device is invalid");
    return zx::error(ZX_ERR_BAD_HANDLE);
  }

  fdf::Arena arena(kArenaTag);
  fdf::WireUnownedResult result = fdf::WireCall(engine_client).buffer(arena)->IsAvailable();
  if (!result.ok()) {
    FDF_LOG(WARNING, "Display engine FIDL device is not available: %s",
            result.FormatDescription().c_str());
    return zx::error(result.status());
  }

  fbl::AllocChecker alloc_checker;
  auto engine_driver_client =
      fbl::make_unique_checked<EngineDriverClient>(&alloc_checker, std::move(engine_client));
  if (!alloc_checker.check()) {
    FDF_LOG(WARNING, "Failed to allocate memory for EngineDriverClient");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(engine_driver_client));
}

zx::result<std::unique_ptr<EngineDriverClient>> CreateBanjoEngineDriverClient(
    std::shared_ptr<fdf::Namespace> incoming) {
  zx::result dc_result = compat::ConnectBanjo<ddk::DisplayEngineProtocolClient>(incoming);
  if (dc_result.is_error()) {
    FDF_LOG(WARNING, "Failed to connect to Banjo server via the compat client: %s",
            dc_result.status_string());
  }
  ddk::DisplayEngineProtocolClient dc = std::move(dc_result).value();
  if (!dc.is_valid()) {
    FDF_LOG(WARNING, "Failed to get Banjo display controller protocol");
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  fbl::AllocChecker alloc_checker;
  auto engine_driver_client =
      fbl::make_unique_checked<EngineDriverClient>(&alloc_checker, std::move(dc));
  if (!alloc_checker.check()) {
    FDF_LOG(WARNING, "Failed to allocate memory for EngineDriverClient");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(engine_driver_client));
}

}  // namespace

// static
zx::result<std::unique_ptr<EngineDriverClient>> EngineDriverClient::Create(
    std::shared_ptr<fdf::Namespace> incoming) {
  ZX_DEBUG_ASSERT(incoming != nullptr);

  // Attempt to connect to FIDL protocol.
  zx::result<std::unique_ptr<EngineDriverClient>> fidl_engine_driver_client_result =
      CreateFidlEngineDriverClient(*incoming);
  if (fidl_engine_driver_client_result.is_ok()) {
    FDF_LOG(INFO, "Using the FIDL Engine driver client");
    return fidl_engine_driver_client_result.take_value();
  }
  FDF_LOG(WARNING, "Failed to create FIDL Engine driver client: %s; fallback to banjo",
          fidl_engine_driver_client_result.status_string());

  // Fallback to Banjo protocol.
  zx::result<std::unique_ptr<EngineDriverClient>> banjo_engine_driver_client_result =
      CreateBanjoEngineDriverClient(incoming);
  if (banjo_engine_driver_client_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create banjo Engine driver client: %s",
            banjo_engine_driver_client_result.status_string());
  }
  return banjo_engine_driver_client_result;
}

EngineDriverClient::EngineDriverClient(ddk::DisplayEngineProtocolClient banjo_engine)
    : use_engine_(false), banjo_engine_(banjo_engine) {
  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
}

EngineDriverClient::EngineDriverClient(
    fdf::ClientEnd<fuchsia_hardware_display_engine::Engine> fidl_engine)
    : use_engine_(true), fidl_engine_(std::move(fidl_engine)) {
  ZX_DEBUG_ASSERT(fidl_engine_.is_valid());
}

EngineDriverClient::~EngineDriverClient() {
  FDF_LOG(TRACE, "EngineDriverClient::~EngineDriverClient");
}

void EngineDriverClient::ReleaseImage(DriverImageId driver_image_id) {
  if (use_engine_) {
    fdf::Arena arena(kArenaTag);
    fdf::WireUnownedResult result =
        fidl_engine_.buffer(arena)->ReleaseImage(ToFidlDriverImageId(driver_image_id));
    if (!result.ok()) {
      FDF_LOG(ERROR, "ReleaseImage failed: %s", result.status_string());
    }
    return;
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  banjo_engine_.ReleaseImage(ToBanjoDriverImageId(driver_image_id));
}

zx::result<> EngineDriverClient::ReleaseCapture(DriverCaptureImageId driver_capture_image_id) {
  if (use_engine_) {
    fdf::Arena arena(kArenaTag);
    fdf::WireUnownedResult result = fidl_engine_.buffer(arena)->ReleaseCapture(
        ToFidlDriverCaptureImageId(driver_capture_image_id));
    return zx::make_result(result.status());
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  zx_status_t banjo_status =
      banjo_engine_.ReleaseCapture(ToBanjoDriverCaptureImageId(driver_capture_image_id));
  return zx::make_result(banjo_status);
}

config_check_result_t EngineDriverClient::CheckConfiguration(
    const display_config_t* display_config_list, size_t display_config_count,
    client_composition_opcode_t* out_client_composition_opcodes_list,
    size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual) {
  if (use_engine_) {
    return CONFIG_CHECK_RESULT_UNSUPPORTED_MODES;
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  return banjo_engine_.CheckConfiguration(
      display_config_list, display_config_count, out_client_composition_opcodes_list,
      client_composition_opcodes_count, out_client_composition_opcodes_actual);
}

void EngineDriverClient::ApplyConfiguration(const display_config_t* display_config_list,
                                            size_t display_config_count,
                                            const config_stamp_t* config_stamp) {
  if (use_engine_) {
    return;
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  banjo_engine_.ApplyConfiguration(display_config_list, display_config_count, config_stamp);
}

void EngineDriverClient::RegisterDisplayEngineListener(
    const display_engine_listener_protocol_t& protocol) {
  if (use_engine_) {
    return;
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  banjo_engine_.RegisterDisplayEngineListener(protocol.ctx, protocol.ops);
}

void EngineDriverClient::DeregisterDisplayEngineListener() {
  if (use_engine_) {
    return;
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  banjo_engine_.DeregisterDisplayEngineListener();
}

zx::result<DriverImageId> EngineDriverClient::ImportImage(const ImageMetadata& image_metadata,
                                                          DriverBufferCollectionId collection_id,
                                                          uint32_t index) {
  if (use_engine_) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  const image_metadata_t banjo_image_metadata = image_metadata.ToBanjo();
  uint64_t image_handle = 0;
  zx_status_t banjo_status = banjo_engine_.ImportImage(
      &banjo_image_metadata, ToBanjoDriverBufferCollectionId(collection_id), index, &image_handle);
  if (banjo_status != ZX_OK) {
    return zx::error(banjo_status);
  }
  return zx::ok(DriverImageId(image_handle));
}

zx::result<DriverCaptureImageId> EngineDriverClient::ImportImageForCapture(
    DriverBufferCollectionId collection_id, uint32_t index) {
  if (use_engine_) {
    fdf::Arena arena(kArenaTag);
    fdf::WireUnownedResult result =
        fidl_engine_.buffer(arena)->ImportImageForCapture(ToFidlDriverBufferId({
            .buffer_collection_id = collection_id,
            .buffer_index = index,
        }));
    if (!result.ok()) {
      return zx::error(result.status());
    }
    if (result->is_error()) {
      return zx::error(result->error_value());
    }
    fuchsia_hardware_display_engine::wire::ImageId image_id = result->value()->capture_image_id;
    return zx::ok(ToDriverCaptureImageId(image_id.value));
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  uint64_t banjo_capture_image_handle = 0;
  zx_status_t banjo_status = banjo_engine_.ImportImageForCapture(
      ToBanjoDriverBufferCollectionId(collection_id), index, &banjo_capture_image_handle);
  if (banjo_status != ZX_OK) {
    return zx::error(banjo_status);
  }
  return zx::ok(ToDriverCaptureImageId(banjo_capture_image_handle));
}

zx::result<> EngineDriverClient::ImportBufferCollection(
    DriverBufferCollectionId collection_id,
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> collection_token) {
  if (use_engine_) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  zx_status_t banjo_status = banjo_engine_.ImportBufferCollection(
      ToBanjoDriverBufferCollectionId(collection_id), std::move(collection_token).TakeChannel());
  return zx::make_result(banjo_status);
}

zx::result<> EngineDriverClient::ReleaseBufferCollection(DriverBufferCollectionId collection_id) {
  if (use_engine_) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  zx_status_t banjo_status =
      banjo_engine_.ReleaseBufferCollection(ToBanjoDriverBufferCollectionId(collection_id));
  return zx::make_result(banjo_status);
}

zx::result<> EngineDriverClient::SetBufferCollectionConstraints(
    const ImageBufferUsage& usage, DriverBufferCollectionId collection_id) {
  if (use_engine_) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  const image_buffer_usage_t banjo_usage = ToBanjoImageBufferUsage(usage);
  zx_status_t banjo_status = banjo_engine_.SetBufferCollectionConstraints(
      &banjo_usage, ToBanjoDriverBufferCollectionId(collection_id));
  return zx::make_result(banjo_status);
}

bool EngineDriverClient::IsCaptureSupported() {
  if (use_engine_) {
    return false;
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  return banjo_engine_.IsCaptureSupported();
}

zx::result<> EngineDriverClient::StartCapture(DriverCaptureImageId driver_capture_image_id) {
  if (use_engine_) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  zx_status_t banjo_status =
      banjo_engine_.StartCapture(ToBanjoDriverCaptureImageId(driver_capture_image_id));
  return zx::make_result(banjo_status);
}

zx::result<> EngineDriverClient::SetDisplayPower(DisplayId display_id, bool power_on) {
  if (use_engine_) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  zx_status_t banjo_status = banjo_engine_.SetDisplayPower(ToBanjoDisplayId(display_id), power_on);
  return zx::make_result(banjo_status);
}

zx::result<> EngineDriverClient::SetMinimumRgb(uint8_t minimum_rgb) {
  if (use_engine_) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  ZX_DEBUG_ASSERT(banjo_engine_.is_valid());
  zx_status_t banjo_status = banjo_engine_.SetMinimumRgb(minimum_rgb);
  return zx::make_result(banjo_status);
}

}  // namespace display
