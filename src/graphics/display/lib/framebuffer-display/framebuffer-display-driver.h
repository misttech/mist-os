// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_FRAMEBUFFER_DISPLAY_FRAMEBUFFER_DISPLAY_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_LIB_FRAMEBUFFER_DISPLAY_FRAMEBUFFER_DISPLAY_DRIVER_H_

#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/mmio/mmio-buffer.h>

#include <memory>

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-banjo-adapter.h"
#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-banjo.h"
#include "src/graphics/display/lib/framebuffer-display/framebuffer-display.h"

namespace framebuffer_display {

// Integration between display drivers and the Driver Framework (v1).
class FramebufferDisplayDriver : public fdf::DriverBase {
 public:
  explicit FramebufferDisplayDriver(std::string_view device_name, fdf::DriverStartArgs start_args,
                                    fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  FramebufferDisplayDriver(const FramebufferDisplayDriver&) = delete;
  FramebufferDisplayDriver(FramebufferDisplayDriver&&) = delete;
  FramebufferDisplayDriver& operator=(const FramebufferDisplayDriver&) = delete;
  FramebufferDisplayDriver& operator=(FramebufferDisplayDriver&&) = delete;

  virtual ~FramebufferDisplayDriver();

  // fdf::DriverBase:
  zx::result<> Start() override;
  void Stop() override;

  // Called exactly once before the driver acquires any resource.
  virtual zx::result<> ConfigureHardware() = 0;

  virtual zx::result<fdf::MmioBuffer> GetFrameBufferMmioBuffer() = 0;

  virtual zx::result<DisplayProperties> GetDisplayProperties() = 0;

 private:
  zx::result<std::unique_ptr<FramebufferDisplay>> CreateAndInitializeFramebufferDisplay();

  // Must be called after `framebuffer_display_` is initialized.
  zx::result<> InitializeBanjoServerNode();

  // Must outlive `framebuffer_display_` and `engine_banjo_adapter_`.
  display::DisplayEngineEventsBanjo engine_events_;

  fdf::SynchronizedDispatcher framebuffer_display_dispatcher_;
  std::unique_ptr<FramebufferDisplay> framebuffer_display_;

  std::unique_ptr<display::DisplayEngineBanjoAdapter> engine_banjo_adapter_;

  compat::SyncInitializedDeviceServer compat_server_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> node_controller_;
};

}  // namespace framebuffer_display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_FRAMEBUFFER_DISPLAY_FRAMEBUFFER_DISPLAY_DRIVER_H_
