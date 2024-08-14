// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/fake-sysmem-device-hierarchy.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <zircon/status.h>

#include <fbl/alloc_checker.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/sysmem/drivers/sysmem/allocator.h"
#include "src/devices/sysmem/drivers/sysmem/device.h"

namespace display {

zx::result<std::unique_ptr<FakeSysmemDeviceHierarchy>> FakeSysmemDeviceHierarchy::Create() {
  return zx::ok(std::make_unique<FakeSysmemDeviceHierarchy>());
}

FakeSysmemDeviceHierarchy::FakeSysmemDeviceHierarchy() {
  zx::result<fdf_testing::TestNode::CreateStartArgsResult> create_start_args_result =
      node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
  ZX_ASSERT_MSG(create_start_args_result.is_ok(), "%s", create_start_args_result.status_string());

  auto [start_args, incoming_directory_server, outgoing_directory_client] =
      std::move(create_start_args_result).value();
  start_args.config() = sysmem_config::Config{}.ToVmo();
  driver_outgoing_ = std::move(outgoing_directory_client);

  auto init_result = test_environment_.SyncCall(&fdf_testing::TestEnvironment::Initialize,
                                                std::move(incoming_directory_server));
  ZX_ASSERT_MSG(init_result.is_ok(), "%s", init_result.status_string());

  fake_pdev::FakePDevFidl::Config config;

  config.use_fake_bti = true;
  fake_pdev_.SyncCall(&fake_pdev::FakePDevFidl::SetConfig, std::move(config));

  const fuchsia_hardware_sysmem::Metadata kSysmemMetadata = [] {
    fuchsia_hardware_sysmem::Metadata metadata;
    metadata.vid() = PDEV_VID_QEMU;
    metadata.pid() = PDEV_PID_QEMU;
    return metadata;
  }();
  fit::result<fidl::Error, std::vector<uint8_t>> metadata_result = fidl::Persist(kSysmemMetadata);
  ZX_ASSERT_MSG(metadata_result.is_ok(), "%s",
                metadata_result.error_value().FormatDescription().c_str());
  std::unordered_map<uint32_t, std::vector<uint8_t>> metadata_map;
  metadata_map.insert(
      {fuchsia_hardware_sysmem::wire::kMetadataType, std::move(metadata_result).value()});
  fake_pdev_.SyncCall(&fake_pdev::FakePDevFidl::set_metadata, std::move(metadata_map));

  auto pdev_instance_handler = fake_pdev_.SyncCall(&fake_pdev::FakePDevFidl::GetInstanceHandler,
                                                   async_patterns::PassDispatcher);
  test_environment_.SyncCall([&pdev_instance_handler](fdf_testing::TestEnvironment* env) {
    auto add_service_result =
        env->incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
            std::move(pdev_instance_handler));
    ZX_ASSERT_MSG(add_service_result.is_ok(), "%s", add_service_result.status_string());
  });

  zx::result start_result = runtime_->RunToCompletion(device_.SyncCall(
      &fdf_testing::DriverUnderTest<sysmem_driver::Device>::Start, std::move(start_args)));
  ZX_ASSERT_MSG(start_result.is_ok(), "%s", start_result.status_string());
}

zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>>
FakeSysmemDeviceHierarchy::ConnectAllocator() {
  auto [client, server] = fidl::Endpoints<fuchsia_sysmem::Allocator>::Create();
  device_.SyncCall([request = std::move(server)](
                       fdf_testing::DriverUnderTest<sysmem_driver::Device>* device) mutable {
    sysmem_driver::Device* dev = **device;
    dev->PostTask([request = std::move(request), dev]() mutable {
      sysmem_driver::Allocator::CreateOwnedV1(std::move(request), dev, dev->v1_allocators());
    });
  });

  return zx::ok(std::move(client));
}

zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>>
FakeSysmemDeviceHierarchy::ConnectAllocator2() {
  auto [client, server] = fidl::Endpoints<fuchsia_sysmem2::Allocator>::Create();
  device_.SyncCall([request = std::move(server)](
                       fdf_testing::DriverUnderTest<sysmem_driver::Device>* device) mutable {
    sysmem_driver::Device* dev = **device;
    dev->PostTask([request = std::move(request), dev]() mutable {
      sysmem_driver::Allocator::CreateOwnedV2(std::move(request), dev, dev->v2_allocators());
    });
  });

  return zx::ok(std::move(client));
}

zx::result<fidl::ClientEnd<fuchsia_hardware_sysmem::Sysmem>>
FakeSysmemDeviceHierarchy::ConnectHardwareSysmem() {
  auto [client, server] = fidl::Endpoints<fuchsia_hardware_sysmem::Sysmem>::Create();
  runtime_->PerformBlockingWork([this, request = std::move(server)]() mutable {
    device_.SyncCall([request = std::move(request)](
                         fdf_testing::DriverUnderTest<sysmem_driver::Device>* device) mutable {
      sysmem_driver::Device* dev = **device;
      dev->BindingsForTest().AddBinding(dev->loop_dispatcher(), std::move(request), dev,
                                        fidl::kIgnoreBindingClosure);
    });
  });

  return zx::ok(std::move(client));
}

FakeSysmemDeviceHierarchy::~FakeSysmemDeviceHierarchy() {
  zx::result prepare_stop_result = runtime_->RunToCompletion(
      device_.SyncCall(&fdf_testing::DriverUnderTest<sysmem_driver::Device>::PrepareStop));
  ZX_ASSERT_MSG(prepare_stop_result.is_ok(), "%s", prepare_stop_result.status_string());

  zx::result stop_result =
      device_.SyncCall(&fdf_testing::DriverUnderTest<sysmem_driver::Device>::Stop);
  ZX_ASSERT_MSG(stop_result.is_ok(), "%s", stop_result.status_string());

  test_environment_.reset();
  fake_pdev_.reset();
  node_server_.reset();
  device_.reset();
}

}  // namespace display
