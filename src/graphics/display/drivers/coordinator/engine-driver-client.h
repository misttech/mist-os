// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_DRIVER_CLIENT_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_DRIVER_CLIENT_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/driver/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/zx/result.h>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>

#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/image-buffer-usage.h"
#include "src/graphics/display/lib/api-types-cpp/image-metadata.h"

namespace display {

static constexpr fdf_arena_tag_t kArenaTag = 'DISP';

class Controller;

// C++ <-> Banjo/FIDL bridge for a connection to a display engine driver.
class EngineDriverClient {
 public:
  EngineDriverClient();

  EngineDriverClient(const EngineDriverClient&) = delete;
  EngineDriverClient& operator=(const EngineDriverClient&) = delete;

  ~EngineDriverClient();

  // TODO(https://fxbug.dev/338002075): Replace the multi-step initialization
  // with a factory function.
  zx_status_t Bind(zx_device_t* parent);

  void ReleaseImage(DriverImageId driver_image_id);
  zx::result<> ReleaseCapture(DriverCaptureImageId driver_capture_image_id);

  config_check_result_t CheckConfiguration(
      const display_config_t* display_config_list, size_t display_config_count,
      client_composition_opcode_t* out_client_composition_opcodes_list,
      size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual);
  void ApplyConfiguration(const display_config_t* display_config_list, size_t display_config_count,
                          const config_stamp_t* config_stamp);

  void SetEld(DisplayId display_id, cpp20::span<const uint8_t> raw_eld);

  // TODO(https://fxbug.dev/314126494): These methods are only used in the
  // banjo transport. Remove when all drivers are migrated to FIDL transport.
  void SetDisplayControllerInterface(const display_controller_interface_protocol_t& protocol);
  void ResetDisplayControllerInterface();

  zx::result<DriverImageId> ImportImage(const ImageMetadata& image_metadata,
                                        DriverBufferCollectionId collection_id, uint32_t index);
  zx::result<DriverCaptureImageId> ImportImageForCapture(DriverBufferCollectionId collection_id,
                                                         uint32_t index);
  zx::result<> ImportBufferCollection(
      DriverBufferCollectionId collection_id,
      fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken> collection_token);
  zx::result<> ReleaseBufferCollection(DriverBufferCollectionId collection_id);
  zx::result<> SetBufferCollectionConstraints(const ImageBufferUsage& usage,
                                              DriverBufferCollectionId collection_id);

  bool IsCaptureSupported();
  zx::result<> StartCapture(DriverCaptureImageId driver_capture_image_id);
  zx::result<> SetDisplayPower(DisplayId display_id, bool power_on);
  zx::result<> SetMinimumRgb(uint8_t minimum_rgb);

  bool is_bound() const { return is_bound_; }

 private:
  // TODO(https://fxbug.dev/325474586): Revisit whether a single arena is the
  // right approach.
  fdf::Arena arena_;

  // FIDL Client
  fdf::WireSyncClient<fuchsia_hardware_display_engine::Engine> engine_;

  // Whether to use the FIDL client. If false, use the Banjo client.
  bool use_engine_ = false;

  // Banjo Client
  ddk::DisplayControllerImplProtocolClient dc_;

  bool is_bound_ = false;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_ENGINE_DRIVER_CLIENT_H_
