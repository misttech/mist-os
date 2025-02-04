// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_ENGINE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_ENGINE_H_

#include <fidl/fuchsia.hardware.goldfish.pipe/cpp/wire.h>
#include <fidl/fuchsia.hardware.goldfish/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/async/cpp/wait.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fzl/pinned-vmo.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/result.h>
#include <threads.h>
#include <zircon/types.h>

#include <list>
#include <memory>

#include <fbl/mutex.h>

#include "src/graphics/display/drivers/goldfish-display/render_control.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"

namespace goldfish {

class DisplayEngine : public ddk::DisplayEngineProtocol<DisplayEngine> {
 public:
  // `control`, `pipe`, `sysmem_allocator` must be valid.
  // `render_control` must not be null.
  // `display_event_dispatcher` must be non-null and outlive `DisplayEngine`.
  explicit DisplayEngine(fidl::ClientEnd<fuchsia_hardware_goldfish::ControlDevice> control,
                         fidl::ClientEnd<fuchsia_hardware_goldfish_pipe::GoldfishPipe> pipe,
                         fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_allocator,
                         std::unique_ptr<RenderControl> render_control,
                         async_dispatcher_t* display_event_dispatcher);

  DisplayEngine(const DisplayEngine&) = delete;
  DisplayEngine(DisplayEngine&&) = delete;
  DisplayEngine& operator=(const DisplayEngine&) = delete;
  DisplayEngine& operator=(DisplayEngine&&) = delete;

  ~DisplayEngine();

  // Performs initialization that cannot be done in the constructor.
  zx::result<> Initialize();

  // Display controller protocol implementation.
  void DisplayEngineSetListener(const display_engine_listener_protocol_t* engine_listener);
  void DisplayEngineUnsetListener();
  zx_status_t DisplayEngineImportBufferCollection(uint64_t banjo_driver_buffer_collection_id,
                                                  zx::channel collection_token);
  zx_status_t DisplayEngineReleaseBufferCollection(uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayEngineImportImage(const image_metadata_t* image_metadata,
                                       uint64_t banjo_driver_buffer_collection_id, uint32_t index,
                                       uint64_t* out_image_handle);
  zx_status_t DisplayEngineImportImageForCapture(uint64_t banjo_driver_buffer_collection_id,
                                                 uint32_t index, uint64_t* out_capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  void DisplayEngineReleaseImage(uint64_t image_handle);
  config_check_result_t DisplayEngineCheckConfiguration(
      const display_config_t* display_config_ptr,
      layer_composition_operations_t* out_layer_composition_operations_list,
      size_t layer_composition_operations_count, size_t* out_layer_composition_operations_actual);
  void DisplayEngineApplyConfiguration(const display_config_t* display_config_ptr,
                                       const config_stamp_t* banjo_config_stamp);
  zx_status_t DisplayEngineSetBufferCollectionConstraints(
      const image_buffer_usage_t* usage, uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayEngineSetDisplayPower(uint64_t display_id, bool power_on) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  bool DisplayEngineIsCaptureSupported() { return false; }
  zx_status_t DisplayEngineStartCapture(uint64_t capture_handle) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t DisplayEngineReleaseCapture(uint64_t capture_handle) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t DisplayEngineSetMinimumRgb(uint8_t minimum_rgb) { return ZX_ERR_NOT_SUPPORTED; }

  // TODO(https://fxbug.dev/42080631): Remove these transitional overloads.
  config_check_result_t DisplayEngineCheckConfiguration(
      const display_config_t* banjo_display_configs_array, size_t banjo_display_configs_count,
      layer_composition_operations_t* out_layer_composition_operations_list,
      size_t out_layer_composition_operations_size,
      size_t* out_layer_composition_operations_actual);
  void DisplayEngineApplyConfiguration(const display_config_t* banjo_display_configs_array,
                                       size_t banjo_display_configs_count,
                                       const config_stamp_t* banjo_config_stamp);

  void SetupPrimaryDisplayForTesting(int32_t width_px, int32_t height_px, int32_t refresh_rate_hz);

  const display_engine_protocol_ops_t* display_engine_protocol_ops() const {
    return &display_engine_protocol_ops_;
  }

 private:
  struct ColorBuffer {
    ~ColorBuffer() = default;

    HostColorBufferId host_color_buffer_id = kInvalidHostColorBufferId;
    size_t size = 0;
    uint32_t width = 0;
    uint32_t height = 0;
    uint32_t format = 0;

    // TODO(costan): Rename to reflect ownership.
    bool is_linear_format = false;

    zx::vmo vmo;
    fzl::PinnedVmo pinned_vmo;
  };

  struct DisplayConfig {
    // For displays with image framebuffer attached to the display, the
    // framebuffer is represented as a |ColorBuffer| in goldfish graphics
    // device implementation.
    // A configuration with a non-null |color_buffer| field means that it will
    // present this |ColorBuffer| image at Vsync; the |ColorBuffer| instance
    // will be created when importing the image and destroyed when releasing
    // the image or removing the display device. Otherwise, it means the display
    // has no framebuffers to display.
    ColorBuffer* color_buffer = nullptr;

    // The |config_stamp| value of the ApplyConfiguration() call to which this
    // DisplayConfig corresponds.
    display::DriverConfigStamp config_stamp = display::kInvalidDriverConfigStamp;
  };

  // TODO(https://fxbug.dev/335324453): Define DisplayState as a class with
  // proper rep invariants on each config update / config flush.
  struct DisplayState {
    int32_t width_px = 0;
    int32_t height_px = 0;
    int32_t refresh_rate_hz = 60;

    zx::time expected_next_flush = zx::time::infinite_past();
    display::DriverConfigStamp latest_config_stamp = display::kInvalidDriverConfigStamp;

    // The next display config to be posted through renderControl protocol.
    std::optional<DisplayConfig> incoming_config = std::nullopt;

    // Queues the async wait of goldfish sync device for each frame that is
    // posted (rendered) but hasn't finished rendering.
    //
    // Every time there's a new frame posted through renderControl protocol,
    // a WaitOnce waiting on the sync event for the latest config will be
    // appended to the queue. When a frame has finished rendering on host, all
    // the pending Waits that are queued no later than the frame's async Wait
    // (including the frame's Wait itself) will be popped out from the queue
    // and destroyed.
    std::list<async::WaitOnce> pending_config_waits;
  };

  zx::result<display::DriverImageId> ImportVmoImage(
      const image_metadata_t& image_metadata, const fuchsia_images2::PixelFormat& pixel_format,
      zx::vmo vmo, size_t offset);

  zx_status_t SetupPrimaryDisplay();
  zx_status_t PresentPrimaryDisplayConfig(const DisplayConfig& display_config);
  void FlushPrimaryDisplay(async_dispatcher_t* dispatcher);

  fbl::Mutex lock_;
  fidl::WireSyncClient<fuchsia_hardware_goldfish::ControlDevice> control_ TA_GUARDED(lock_);
  fidl::WireSyncClient<fuchsia_hardware_goldfish_pipe::GoldfishPipe> pipe_ TA_GUARDED(lock_);

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem_allocator_client_;

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::SyncClient<fuchsia_sysmem2::BufferCollection>>
      buffer_collections_;

  std::unique_ptr<RenderControl> rc_;
  DisplayState primary_display_device_ = {};
  fbl::Mutex flush_lock_;
  ddk::DisplayEngineListenerProtocolClient engine_listener_ TA_GUARDED(flush_lock_);

  async_dispatcher_t* const display_event_dispatcher_;
};

}  // namespace goldfish

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_ENGINE_H_
