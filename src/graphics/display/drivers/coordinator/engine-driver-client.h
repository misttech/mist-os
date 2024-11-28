// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_DRIVER_CLIENT_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_DRIVER_CLIENT_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/driver/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/zx/result.h>

#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types/cpp/image-metadata.h"

namespace display_coordinator {

class Controller;

// C++ <-> Banjo/FIDL bridge for a connection to a display engine driver.
class EngineDriverClient {
 public:
  // Factory method for production use.
  // `parent` must be valid.
  static zx::result<std::unique_ptr<EngineDriverClient>> Create(zx_device_t* parent);

  static zx::result<std::unique_ptr<EngineDriverClient>> Create(
      std::shared_ptr<fdf::Namespace> incoming);

  // Production code must use the Create() factory method.
  // `banjo_engine` must be valid.
  explicit EngineDriverClient(ddk::DisplayEngineProtocolClient banjo_engine);

  // Production code must use the Create() factory method.
  // `fidl_engine` must be valid.
  explicit EngineDriverClient(fdf::ClientEnd<fuchsia_hardware_display_engine::Engine> fidl_engine);

  EngineDriverClient(const EngineDriverClient&) = delete;
  EngineDriverClient& operator=(const EngineDriverClient&) = delete;

  ~EngineDriverClient();

  void ReleaseImage(display::DriverImageId driver_image_id);
  zx::result<> ReleaseCapture(display::DriverCaptureImageId driver_capture_image_id);

  config_check_result_t CheckConfiguration(
      const display_config_t* display_config_list, size_t display_config_count,
      client_composition_opcode_t* out_client_composition_opcodes_list,
      size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual);
  void ApplyConfiguration(const display_config_t* display_config_list, size_t display_config_count,
                          const config_stamp_t* config_stamp);

  // TODO(https://fxbug.dev/314126494): These methods are only used in the
  // banjo transport. Remove when all drivers are migrated to FIDL transport.
  void RegisterDisplayEngineListener(const display_engine_listener_protocol_t& protocol);
  void DeregisterDisplayEngineListener();

  zx::result<display::DriverImageId> ImportImage(const display::ImageMetadata& image_metadata,
                                                 display::DriverBufferCollectionId collection_id,
                                                 uint32_t index);
  zx::result<display::DriverCaptureImageId> ImportImageForCapture(
      display::DriverBufferCollectionId collection_id, uint32_t index);
  zx::result<> ImportBufferCollection(
      display::DriverBufferCollectionId collection_id,
      fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> collection_token);
  zx::result<> ReleaseBufferCollection(display::DriverBufferCollectionId collection_id);
  zx::result<> SetBufferCollectionConstraints(const display::ImageBufferUsage& usage,
                                              display::DriverBufferCollectionId collection_id);

  bool IsCaptureSupported();
  zx::result<> StartCapture(display::DriverCaptureImageId driver_capture_image_id);
  zx::result<> SetDisplayPower(display::DisplayId display_id, bool power_on);
  zx::result<> SetMinimumRgb(uint8_t minimum_rgb);

 private:
  // Whether to use the FIDL client. If false, use the Banjo client.
  bool use_engine_;

  // FIDL Client
  fdf::WireSyncClient<fuchsia_hardware_display_engine::Engine> fidl_engine_;

  // Banjo Client
  ddk::DisplayEngineProtocolClient banjo_engine_;
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_DRIVER_CLIENT_H_
