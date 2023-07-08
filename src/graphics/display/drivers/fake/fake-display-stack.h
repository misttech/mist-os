// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_DISPLAY_STACK_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_DISPLAY_STACK_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/sysmem/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>

#include <memory>
#include <optional>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/fake/fake-display.h"
#include "src/graphics/display/drivers/fake/sysmem-device-wrapper.h"

namespace display {

// FakeDisplayStack creates and holds a FakeDisplay device as well as the
// Sysmem device and the display coordinator Controller which are attached to
// the fake display device and clients can connect to.
class FakeDisplayStack {
 public:
  // |sysmem| allows the caller to customize the sysmem implementation used by the
  // FakeDisplayStack.  See SysmemDeviceWrapper for more details, as well as existing
  // specializations of GenericSysmemDeviceWrapper<>.
  FakeDisplayStack(std::shared_ptr<zx_device> mock_root,
                   std::unique_ptr<SysmemDeviceWrapper> sysmem,
                   const fake_display::FakeDisplayDeviceConfig& device_config);
  ~FakeDisplayStack();

  Controller* coordinator_controller() { return coordinator_controller_; }
  fake_display::FakeDisplay* display() { return display_; }

  const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& display_client();
  const fidl::WireSyncClient<fuchsia_sysmem2::DriverConnector>& sysmem_client();

  // Join all threads providing display and sysmem protocols, and remove all
  // the devices bound to the mock root device.
  void SyncShutdown();

 private:
  fidl::ClientEnd<fuchsia_io::Directory> SetUpPDevFidlServer();

  std::shared_ptr<zx_device> mock_root_;

  fake_pdev::FakePDevFidl pdev_fidl_;

  std::unique_ptr<SysmemDeviceWrapper> sysmem_;

  // Fake devices created as descendents of the root MockDevice.
  // All the devices have transferred their ownership to `mock_root_` and will
  // be torn down on `SyncShutdown()`.
  fake_display::FakeDisplay* display_;
  Controller* coordinator_controller_;
  zx_device_t* sysmem_device_;

  bool shutdown_ = false;

  const sysmem_metadata_t sysmem_metadata_ = {
      .vid = PDEV_VID_QEMU,
      .pid = PDEV_PID_QEMU,
      .protected_memory_size = 0,
      .contiguous_memory_size = 0,
  };

  // Runs services provided by the fake display and display coordinator driver.
  // Must be torn down before `display_` and `coordinator_controller_` is
  // removed.
  async::Loop display_loop_{&kAsyncLoopConfigNeverAttachToThread};
  // Runs services provided by the sysmem driver. Must be torn down before
  // `sysmem_device_` is removed.
  async::Loop sysmem_loop_{&kAsyncLoopConfigNeverAttachToThread};
  // Runs services provided by the fake platform device (pdev). Must be torn
  // down before `pdev_fidl_`.
  async::Loop pdev_loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::optional<component::OutgoingDirectory> outgoing_;

  fidl::WireSyncClient<fuchsia_hardware_display::Provider> display_provider_client_;
  fidl::WireSyncClient<fuchsia_sysmem2::DriverConnector> sysmem_client_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_DISPLAY_STACK_H_
