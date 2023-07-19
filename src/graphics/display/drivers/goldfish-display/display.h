// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_H_

#include <fidl/fuchsia.hardware.goldfish.pipe/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <fuchsia/hardware/goldfish/control/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/wait.h>
#include <lib/ddk/device.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fzl/pinned-vmo.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <threads.h>
#include <zircon/types.h>

#include <list>
#include <map>
#include <memory>

#include <ddktl/device.h>
#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

#include "src/devices/lib/goldfish/pipe_io/pipe_io.h"
#include "src/graphics/display/drivers/goldfish-display/render_control.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"

namespace goldfish {

class Display;
using DisplayType = ddk::Device<Display, ddk::ChildPreReleaseable>;

class Display : public DisplayType,
                public ddk::DisplayControllerImplProtocol<Display, ddk::base_protocol> {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  explicit Display(zx_device_t* parent);

  ~Display();

  zx_status_t Bind();

  // Device protocol implementation.
  void DdkRelease();
  void DdkChildPreRelease(void* child_ctx) {
    fbl::AutoLock lock(&flush_lock_);
    dc_intf_ = ddk::DisplayControllerInterfaceProtocolClient();
  }

  // Display controller protocol implementation.
  void DisplayControllerImplSetDisplayControllerInterface(
      const display_controller_interface_protocol_t* interface);
  zx_status_t DisplayControllerImplSetDisplayCaptureInterface(
      const display_capture_interface_protocol_t* intf) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t DisplayControllerImplImportBufferCollection(
      uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token);
  zx_status_t DisplayControllerImplReleaseBufferCollection(
      uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplImportImage(image_t* image,
                                               uint64_t banjo_driver_buffer_collection_id,
                                               uint32_t index);
  zx_status_t DisplayControllerImplImportImageForCapture(uint64_t banjo_driver_buffer_collection_id,
                                                         uint32_t index,
                                                         uint64_t* out_capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  void DisplayControllerImplReleaseImage(image_t* image);
  config_check_result_t DisplayControllerImplCheckConfiguration(
      const display_config_t** display_configs, size_t display_count,
      client_composition_opcode_t* out_client_composition_opcodes_list,
      size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual);
  void DisplayControllerImplApplyConfiguration(const display_config_t** display_config,
                                               size_t display_count,
                                               const config_stamp_t* banjo_config_stamp);
  void DisplayControllerImplSetEld(uint64_t display_id, const uint8_t* raw_eld_list,
                                   size_t raw_eld_count) {}  // No ELD required for non-HDA systems.
  zx_status_t DisplayControllerImplGetSysmemConnection(zx::channel connection);
  zx_status_t DisplayControllerImplSetBufferCollectionConstraints(
      const image_t* config, uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplSetDisplayPower(uint64_t display_id, bool power_on) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t DisplayControllerImplStartCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t DisplayControllerImplReleaseCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  bool DisplayControllerImplIsCaptureCompleted() { return false; }

  void SetSysmemAllocatorForTesting(
      fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_allocator_client) {
    sysmem_allocator_client_ = std::move(sysmem_allocator_client);
  }

  // TESTING ONLY
  void CreateDevices(int num_devices) {
    constexpr uint32_t dummy_width = 1024;
    constexpr uint32_t dummy_height = 768;
    constexpr uint32_t dummy_fr = 60;
    ZX_DEBUG_ASSERT(devices_.empty());
    for (int i = 0; i < num_devices; i++) {
      display::DisplayId display_id(i + 1);
      auto& device = devices_[display_id];
      device.width = dummy_width;
      device.height = dummy_height;
      device.refresh_rate_hz = dummy_fr;
    }
  }
  void RemoveDevices() {
    ZX_DEBUG_ASSERT(!devices_.empty());
    devices_.clear();
    ZX_DEBUG_ASSERT(devices_.empty());
  }

 private:
  struct ColorBuffer {
    ~ColorBuffer() = default;

    HostColorBufferId host_color_buffer_id = kInvalidHostColorBufferId;
    size_t size = 0;
    uint32_t width = 0;
    uint32_t height = 0;
    uint32_t format = 0;
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
    display::ConfigStamp config_stamp = display::kInvalidConfigStamp;
  };

  struct Device {
    uint32_t width = 0;
    uint32_t height = 0;
    uint32_t x = 0;
    uint32_t y = 0;
    uint32_t refresh_rate_hz = 60;
    // Display ID used for host framebuffer rendering. Not to be confused with
    // display::DisplayId or banjo display ID.
    HostDisplayId host_display_id = kInvalidHostDisplayId;
    float scale = 1.0;
    zx::time expected_next_flush = zx::time::infinite_past();
    display::ConfigStamp latest_config_stamp = display::kInvalidConfigStamp;

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
    std::list<async::WaitOnce> pending_config_waits = {};
  };

  // Initializes the sysmem Allocator client used to import incoming buffer
  // collection tokens.
  //
  // On success, returns ZX_OK and the sysmem allocator client will be open
  // until the device is released.
  zx_status_t InitSysmemAllocatorClientLocked() TA_REQ(lock_);

  zx_status_t ImportVmoImage(image_t* image, const fuchsia_sysmem::PixelFormat& pixel_format,
                             zx::vmo vmo, size_t offset);
  zx_status_t PresentDisplayConfig(display::DisplayId display_id,
                                   const DisplayConfig& display_config);
  zx_status_t SetupDisplay(display::DisplayId display_id);

  void TeardownDisplay(display::DisplayId display_id);
  void FlushDisplay(async_dispatcher_t* dispatcher, display::DisplayId display_id);

  fbl::Mutex lock_;
  ddk::GoldfishControlProtocolClient control_ TA_GUARDED(lock_);
  fidl::WireSyncClient<fuchsia_hardware_goldfish_pipe::GoldfishPipe> pipe_ TA_GUARDED(lock_);

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_allocator_client_;

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::SyncClient<fuchsia_sysmem::BufferCollection>>
      buffer_collections_;

  std::unique_ptr<RenderControl> rc_;
  zx::bti bti_;
  ddk::IoBuffer cmd_buffer_ TA_GUARDED(lock_);
  ddk::IoBuffer io_buffer_ TA_GUARDED(lock_);
  std::map<display::DisplayId, Device> devices_;
  fbl::Mutex flush_lock_;
  ddk::DisplayControllerInterfaceProtocolClient dc_intf_ TA_GUARDED(flush_lock_);

  async::Loop loop_;

  DISALLOW_COPY_ASSIGN_AND_MOVE(Display);
};

}  // namespace goldfish

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_GOLDFISH_DISPLAY_DISPLAY_H_
