// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_DRIVER_CLIENT_BANJO_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_DRIVER_CLIENT_BANJO_H_

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/zx/result.h>

#include <cstdint>

#include "src/graphics/display/drivers/coordinator/engine-driver-client.h"
#include "src/graphics/display/lib/api-types/cpp/config-check-result.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/engine-info.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"

namespace display_coordinator {

// C++ <-> Banjo bridge for a connection to a display engine driver.
class EngineDriverClientBanjo : public EngineDriverClient {
 public:
  // `banjo_engine` must be valid.
  explicit EngineDriverClientBanjo(ddk::DisplayEngineProtocolClient banjo_engine);

  EngineDriverClientBanjo(const EngineDriverClientBanjo&) = delete;
  EngineDriverClientBanjo& operator=(const EngineDriverClientBanjo&) = delete;

  ~EngineDriverClientBanjo() override;

  // `EngineDriverClient`:
  void ReleaseImage(display::DriverImageId driver_image_id) override;
  zx::result<> ReleaseCapture(display::DriverCaptureImageId driver_capture_image_id) override;
  display::ConfigCheckResult CheckConfiguration(const display_config_t* display_config) override;
  void ApplyConfiguration(const display_config_t* display_config,
                          display::DriverConfigStamp config_stamp) override;
  display::EngineInfo CompleteCoordinatorConnection(
      const display_engine_listener_protocol_t& protocol) override;
  void UnsetListener() override;
  zx::result<display::DriverImageId> ImportImage(const display::ImageMetadata& image_metadata,
                                                 display::DriverBufferCollectionId collection_id,
                                                 uint32_t index) override;
  zx::result<display::DriverCaptureImageId> ImportImageForCapture(
      display::DriverBufferCollectionId collection_id, uint32_t index) override;
  zx::result<> ImportBufferCollection(
      display::DriverBufferCollectionId collection_id,
      fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> collection_token) override;
  zx::result<> ReleaseBufferCollection(display::DriverBufferCollectionId collection_id) override;
  zx::result<> SetBufferCollectionConstraints(
      const display::ImageBufferUsage& usage,
      display::DriverBufferCollectionId collection_id) override;
  zx::result<> StartCapture(display::DriverCaptureImageId driver_capture_image_id) override;
  zx::result<> SetDisplayPower(display::DisplayId display_id, bool power_on) override;
  zx::result<> SetMinimumRgb(uint8_t minimum_rgb) override;

 private:
  ddk::DisplayEngineProtocolClient banjo_engine_;
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_DRIVER_CLIENT_BANJO_H_
