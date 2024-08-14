// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_SYSMEM_DEVICE_HIERARCHY_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_SYSMEM_DEVICE_HIERARCHY_H_

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/sysmem/drivers/sysmem/device.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/drivers/fake/sysmem-service-provider.h"

namespace display {

class FakeSysmemDeviceHierarchy : public SysmemServiceProvider {
 public:
  static zx::result<std::unique_ptr<FakeSysmemDeviceHierarchy>> Create();

  FakeSysmemDeviceHierarchy();
  ~FakeSysmemDeviceHierarchy() override;

  FakeSysmemDeviceHierarchy(const FakeSysmemDeviceHierarchy&) = delete;
  FakeSysmemDeviceHierarchy& operator=(const FakeSysmemDeviceHierarchy&) = delete;
  FakeSysmemDeviceHierarchy(FakeSysmemDeviceHierarchy&&) = delete;
  FakeSysmemDeviceHierarchy& operator=(FakeSysmemDeviceHierarchy&&) = delete;

  // Initialization logic not suitable for the constructor.
  zx::result<> Initialize();

  // SysmemServiceProvider:
  zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>> ConnectAllocator() override;
  zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>> ConnectAllocator2() override;
  zx::result<fidl::ClientEnd<fuchsia_hardware_sysmem::Sysmem>> ConnectHardwareSysmem() override;

 private:
  fdf_testing::DriverRuntime& runtime() { return *runtime_; }
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<sysmem_driver::Device>>&
  driver() {
    return device_;
  }

  async_dispatcher_t* driver_async_dispatcher() {
    return (*driver_dispatcher_)->async_dispatcher();
  }
  async_dispatcher_t* env_async_dispatcher() { return (*env_dispatcher_)->async_dispatcher(); }

  // Attaches a foreground dispatcher for us automatically.
  std::shared_ptr<fdf_testing::DriverRuntime> runtime_ = mock_ddk::GetDriverRuntime();

  // Env dispatcher and driver dispatchers run separately in the background because we need to make
  // sync calls from driver dispatcher to env dispatcher, and from test thread to driver dispatcher
  // (in Connect).
  std::optional<fdf::UnownedSynchronizedDispatcher> driver_dispatcher_ =
      runtime_->StartBackgroundDispatcher();
  std::optional<fdf::UnownedSynchronizedDispatcher> env_dispatcher_ =
      runtime_->StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_async_dispatcher(), std::in_place, std::string("root")};
  async_patterns::TestDispatcherBound<fake_pdev::FakePDevFidl> fake_pdev_{env_async_dispatcher(),
                                                                          std::in_place};
  async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment_{
      env_async_dispatcher(), std::in_place};

  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<sysmem_driver::Device>> device_{
      driver_async_dispatcher(), std::in_place};

  fuchsia_driver_framework::DriverStartArgs start_args_;
  fidl::ClientEnd<fuchsia_io::Directory> driver_outgoing_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_FAKE_SYSMEM_DEVICE_HIERARCHY_H_
