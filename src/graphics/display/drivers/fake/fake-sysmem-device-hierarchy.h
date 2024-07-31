// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_SYSMEM_DEVICE_HIERARCHY_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_SYSMEM_DEVICE_HIERARCHY_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/zx/result.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/sysmem/drivers/sysmem/driver.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/drivers/fake/sysmem-service-provider.h"

namespace display {

class FakeSysmemDeviceHierarchy : public SysmemServiceProvider {
 public:
  static zx::result<std::unique_ptr<FakeSysmemDeviceHierarchy>> Create();

  explicit FakeSysmemDeviceHierarchy(std::unique_ptr<fake_pdev::FakePDevFidl> fake_pdev);
  ~FakeSysmemDeviceHierarchy() override;

  FakeSysmemDeviceHierarchy(const FakeSysmemDeviceHierarchy&) = delete;
  FakeSysmemDeviceHierarchy& operator=(const FakeSysmemDeviceHierarchy&) = delete;
  FakeSysmemDeviceHierarchy(FakeSysmemDeviceHierarchy&&) = delete;
  FakeSysmemDeviceHierarchy& operator=(FakeSysmemDeviceHierarchy&&) = delete;

  // Initialization logic not suitable for the constructor.
  zx::result<> Initialize();

  // SysmemServiceProvider:
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> GetOutgoingDirectory() override;

 private:
  zx::result<> InitializeIncomingServices();
  zx::result<> CreateAndBindSysmemDevice();
  void SyncShutdown();

  fuchsia_hardware_platform_device::Service::InstanceHandler
  GetPlatformDeviceServiceInstanceHandler() const;

  std::shared_ptr<MockDevice> mock_root_ = MockDevice::FakeRootParent();

  // Serves the `incoming_` service directory and the fake platform device.
  // Must outlive `incoming_` and `fake_pdev_`.
  async::Loop env_loop_{&kAsyncLoopConfigNeverAttachToThread};

  std::unique_ptr<fake_pdev::FakePDevFidl> fake_pdev_;

  // Directory of incoming services to be consumed by the fake sysmem Device.
  // Must be created, called and destroyed on `env_loop_`.
  std::optional<component::OutgoingDirectory> incoming_;

  // Must outlive the created `sysmem_driver::Device` instance managed by the
  // mock ddk.
  sysmem_driver::Driver driver_context_;

  zx_device_t* sysmem_device_ = nullptr;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_SYSMEM_DEVICE_HIERARCHY_H_
