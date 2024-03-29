// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/driver.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/zx/channel.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <cstdint>

#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/image-id.h"

namespace display {

Driver::Driver(Controller* controller, zx_device_t* parent)
    : ddk::Device<Driver>(parent),
      arena_(fdf::Arena(kArenaTag)),
      controller_(controller),
      parent_(parent) {
  ZX_DEBUG_ASSERT(controller);
}

Driver::~Driver() { zxlogf(TRACE, "Driver::~Driver"); }

zx_status_t Driver::Bind() {
  // Attempt to connect to FIDL protocol.
  zx::result display_fidl_client =
      DdkConnectRuntimeProtocol<fuchsia_hardware_display_engine::Service::Engine>(parent_);
  if (display_fidl_client.is_ok()) {
    engine_ = fdf::WireSyncClient(std::move(*display_fidl_client));
    if (engine_.is_valid()) {
      fdf::WireUnownedResult result = engine_.buffer(arena_)->IsAvailable();
      if (result.ok()) {
        zxlogf(INFO, "Using FIDL display controller protocol");
        // If the server responds, use it exclusively.
        use_engine_ = true;
        return ZX_OK;
      }
    }
  }

  // Fallback to Banjo protocol.
  dc_ = ddk::DisplayControllerImplProtocolClient(parent_);
  if (!dc_.is_valid()) {
    zxlogf(ERROR, "Failed to get Banjo or FIDL display controller protocol");
    return ZX_ERR_NOT_SUPPORTED;
  }

  zxlogf(INFO, "Using Banjo display controller protocol");
  return ZX_OK;
}

void Driver::ReleaseImage(image_t* image) {
  if (use_engine_) {
    fdf::WireUnownedResult result =
        engine_.buffer(arena_)->ReleaseImage(ToFidlDriverImageId(DriverImageId(image->handle)));
    if (!result.ok()) {
      zxlogf(ERROR, "ReleaseImage failed: %s", result.status_string());
    }
    return;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  dc_.ReleaseImage(image->handle);
}

zx_status_t Driver::ReleaseCapture(DriverCaptureImageId driver_capture_image_id) {
  if (use_engine_) {
    fdf::WireUnownedResult result =
        engine_.buffer(arena_)->ReleaseCapture(ToFidlDriverCaptureImageId(driver_capture_image_id));
    return result.status();
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  return dc_.ReleaseCapture(ToBanjoDriverCaptureImageId(driver_capture_image_id));
}

config_check_result_t Driver::CheckConfiguration(
    const display_config_t** display_config_list, size_t display_config_count,
    client_composition_opcode_t* out_client_composition_opcodes_list,
    size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual) {
  if (use_engine_) {
    return CONFIG_CHECK_RESULT_UNSUPPORTED_MODES;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  return dc_.CheckConfiguration(
      display_config_list, display_config_count, out_client_composition_opcodes_list,
      client_composition_opcodes_count, out_client_composition_opcodes_actual);
}

void Driver::ApplyConfiguration(const display_config_t** display_config_list,
                                size_t display_config_count, const config_stamp_t* config_stamp) {
  if (use_engine_) {
    return;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  dc_.ApplyConfiguration(display_config_list, display_config_count, config_stamp);
}

void Driver::SetEld(DisplayId display_id, const uint8_t* raw_eld_list, size_t raw_eld_count) {
  if (use_engine_) {
    return;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  dc_.SetEld(ToBanjoDisplayId(display_id), raw_eld_list, raw_eld_count);
}

void Driver::SetDisplayControllerInterface(display_controller_interface_protocol_ops_t* ops) {
  if (use_engine_) {
    return;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  dc_.SetDisplayControllerInterface(controller_, ops);
}

void Driver::ResetDisplayControllerInterface() {
  if (use_engine_) {
    return;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  dc_.ResetDisplayControllerInterface();
}

zx_status_t Driver::ImportImage(image_t* image, DriverBufferCollectionId collection_id,
                                uint32_t index) {
  if (use_engine_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  const image_metadata_t image_metadata = {
      .width = image->width,
      .height = image->height,
      .tiling_type = image->tiling_type,
  };
  uint64_t image_handle = 0;
  zx_status_t status = dc_.ImportImage(
      &image_metadata, ToBanjoDriverBufferCollectionId(collection_id), index, &image_handle);
  image->handle = image_handle;
  return status;
}

zx_status_t Driver::ImportImageForCapture(DriverBufferCollectionId collection_id, uint32_t index,
                                          DriverCaptureImageId* capture_image_id) {
  if (use_engine_) {
    fdf::WireUnownedResult result =
        engine_.buffer(arena_)->ImportImageForCapture(ToFidlDriverBufferId({
            .buffer_collection_id = collection_id,
            .buffer_index = index,
        }));
    if (result.ok()) {
      fuchsia_hardware_display_engine::wire::ImageId image_id = result->value()->capture_image_id;
      *capture_image_id = ToDriverCaptureImageId(image_id.value);
    }
    return result.status();
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  uint64_t banjo_capture_image_id = ToBanjoDriverCaptureImageId(*capture_image_id);
  auto status = dc_.ImportImageForCapture(ToBanjoDriverBufferCollectionId(collection_id), index,
                                          &banjo_capture_image_id);
  *capture_image_id = ToDriverCaptureImageId(banjo_capture_image_id);
  return status;
}

zx_status_t Driver::ImportBufferCollection(DriverBufferCollectionId collection_id,
                                           zx::channel collection_token) {
  if (use_engine_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  return dc_.ImportBufferCollection(ToBanjoDriverBufferCollectionId(collection_id),
                                    std::move(collection_token));
}

zx_status_t Driver::ReleaseBufferCollection(DriverBufferCollectionId collection_id) {
  if (use_engine_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  return dc_.ReleaseBufferCollection(ToBanjoDriverBufferCollectionId(collection_id));
}

zx_status_t Driver::SetBufferCollectionConstraints(const image_buffer_usage_t& usage,
                                                   DriverBufferCollectionId collection_id) {
  if (use_engine_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  return dc_.SetBufferCollectionConstraints(&usage, ToBanjoDriverBufferCollectionId(collection_id));
}

bool Driver::IsCaptureSupported() {
  if (use_engine_) {
    return false;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  return dc_.IsCaptureSupported();
}

zx_status_t Driver::StartCapture(DriverCaptureImageId driver_capture_image_id) {
  if (use_engine_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  return dc_.StartCapture(ToBanjoDriverCaptureImageId(driver_capture_image_id));
}

zx_status_t Driver::SetDisplayPower(DisplayId display_id, bool power_on) {
  if (use_engine_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  return dc_.SetDisplayPower(ToBanjoDisplayId(display_id), power_on);
}

zx_status_t Driver::SetMinimumRgb(uint8_t minimum_rgb) {
  if (use_engine_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  return dc_.SetMinimumRgb(minimum_rgb);
}

zx_status_t Driver::GetSysmemConnection(zx::channel sysmem_handle) {
  if (use_engine_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  ZX_DEBUG_ASSERT(dc_.is_valid());
  return dc_.GetSysmemConnection(std::move(sysmem_handle));
}

}  // namespace display
