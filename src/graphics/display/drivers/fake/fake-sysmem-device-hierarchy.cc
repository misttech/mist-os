// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/fake-sysmem-device-hierarchy.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <zircon/status.h>

#include <fbl/alloc_checker.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/sysmem/drivers/sysmem/device.h"

namespace display {

// static
zx::result<std::unique_ptr<FakeSysmemDeviceHierarchy>> FakeSysmemDeviceHierarchy::Create() {
  fbl::AllocChecker alloc_checker;
  auto fake_pdev = fbl::make_unique_checked<fake_pdev::FakePDevFidl>(&alloc_checker);
  if (!alloc_checker.check()) {
    FX_LOGS(ERROR) << "Failed to allocate memory for FakePDevFidl";
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  fake_pdev->SetConfig({
      .use_fake_bti = true,
  });

  const fuchsia_hardware_sysmem::Metadata kSysmemMetadata{{
      .vid = PDEV_VID_QEMU,
      .pid = PDEV_PID_QEMU,
  }};
  fit::result<fidl::Error, std::vector<uint8_t>> metadata_result = fidl::Persist(kSysmemMetadata);
  if (metadata_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to persist sysmem metadata: "
                   << metadata_result.error_value().FormatDescription();
    return zx::error(metadata_result.error_value().status());
  }
  std::vector<uint8_t> metadata = std::move(metadata_result).value();
  fake_pdev->set_metadata({{fuchsia_hardware_sysmem::wire::kMetadataType, std::move(metadata)}});

  auto fake_sysmem_device_hierarchy =
      fbl::make_unique_checked<FakeSysmemDeviceHierarchy>(&alloc_checker, std::move(fake_pdev));
  if (!alloc_checker.check()) {
    FX_LOGS(ERROR) << "Failed to allocate memory for FakeSysmemDeviceHierarchy";
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> initialize_result = fake_sysmem_device_hierarchy->Initialize();
  if (initialize_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to initialize FakeSysmemDeviceHierarchy: "
                   << initialize_result.status_string();
    return initialize_result.take_error();
  }

  return zx::ok(std::move(fake_sysmem_device_hierarchy));
}

FakeSysmemDeviceHierarchy::FakeSysmemDeviceHierarchy(
    std::unique_ptr<fake_pdev::FakePDevFidl> fake_pdev)
    : fake_pdev_(std::move(fake_pdev)) {
  zx_status_t start_thread_status = env_loop_.StartThread("fake-sysmem-env-loop");
  ZX_ASSERT(start_thread_status == ZX_OK);
}

FakeSysmemDeviceHierarchy::~FakeSysmemDeviceHierarchy() { SyncShutdown(); }

void FakeSysmemDeviceHierarchy::SyncShutdown() {
  device_async_remove(sysmem_device_);
  mock_ddk::ReleaseFlaggedDevices(mock_root_.get());
  sysmem_device_ = nullptr;

  // Ensure all tasks started before this call finish before shutting down the loop.
  async::PostTask(env_loop_.dispatcher(), [this]() {
    incoming_.reset();
    env_loop_.Quit();
  });
  // Waits for the Quit() to execute and cause the thread to exit.
  env_loop_.JoinThreads();
  env_loop_.Shutdown();
}

fuchsia_hardware_platform_device::Service::InstanceHandler
FakeSysmemDeviceHierarchy::GetPlatformDeviceServiceInstanceHandler() const {
  return fuchsia_hardware_platform_device::Service::InstanceHandler{{
      .device = fake_pdev_->bind_handler(env_loop_.dispatcher()),
  }};
}

zx::result<> FakeSysmemDeviceHierarchy::Initialize() {
  zx::result<> initialize_incoming_services_result = InitializeIncomingServices();
  if (initialize_incoming_services_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to initialize sysmem incoming services: "
                   << initialize_incoming_services_result.status_string();
    return initialize_incoming_services_result.take_error();
  }

  zx::result<> create_and_bind_sysmem_device_result = CreateAndBindSysmemDevice();
  if (create_and_bind_sysmem_device_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to create sysmem device: "
                   << create_and_bind_sysmem_device_result.status_string();
    return create_and_bind_sysmem_device_result.take_error();
  }

  return zx::ok();
}

zx::result<> FakeSysmemDeviceHierarchy::InitializeIncomingServices() {
  ZX_DEBUG_ASSERT(!incoming_.has_value());

  libsync::Completion incoming_init_completed;
  async::PostTask(env_loop_.dispatcher(), [&] {
    incoming_ = component::OutgoingDirectory(env_loop_.dispatcher());
    incoming_init_completed.Signal();
  });
  incoming_init_completed.Wait();

  libsync::Completion add_service_completed;
  zx::result<> add_service_result = zx::error(ZX_ERR_INTERNAL);
  async::PostTask(env_loop_.dispatcher(), [&] {
    add_service_result = incoming_->AddService<fuchsia_hardware_platform_device::Service>(
        GetPlatformDeviceServiceInstanceHandler());
    add_service_completed.Signal();
  });
  add_service_completed.Wait();
  if (add_service_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to add pdev service to incoming directory: "
                   << add_service_result.status_string();
    return add_service_result.take_error();
  }

  auto [incoming_root_client, incoming_root_server] =
      fidl::Endpoints<fuchsia_io::Directory>::Create();
  libsync::Completion serve_completed;
  zx::result<> serve_result = zx::error(ZX_ERR_INTERNAL);
  async::PostTask(env_loop_.dispatcher(),
                  [&, incoming_root_server = std::move(incoming_root_server)]() mutable {
                    serve_result = incoming_->Serve(std::move(incoming_root_server));
                    serve_completed.Signal();
                  });
  serve_completed.Wait();
  if (serve_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve the incoming directory: " << serve_result.status_string();
    return serve_result.take_error();
  }

  mock_root_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                             std::move(incoming_root_client));
  return zx::ok();
}

zx::result<> FakeSysmemDeviceHierarchy::CreateAndBindSysmemDevice() {
  ZX_DEBUG_ASSERT(sysmem_device_ == nullptr);
  fbl::AllocChecker alloc_checker;
  auto device = fbl::make_unique_checked<sysmem_driver::Device>(&alloc_checker, mock_root_.get(),
                                                                &driver_context_);
  if (!alloc_checker.check()) {
    FX_LOGS(ERROR) << "Failed to allocate memory for sysmem_driver::Device";
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx_status_t bind_result = device->Bind();
  if (bind_result != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to initialize and bind the sysmem Device: "
                   << zx_status_get_string(bind_result);
    return zx::error(bind_result);
  }

  // `device` is now managed by the mock ddk.
  [[maybe_unused]] sysmem_driver::Device* released_device = device.release();
  sysmem_device_ = mock_root_->GetLatestChild();
  ZX_DEBUG_ASSERT(sysmem_device_ != nullptr);

  return zx::ok();
}

zx::result<fidl::ClientEnd<fuchsia_io::Directory>>
FakeSysmemDeviceHierarchy::GetOutgoingDirectory() {
  ZX_DEBUG_ASSERT(sysmem_device_ != nullptr);
  sysmem_driver::Device* device = sysmem_device_->GetDeviceContext<sysmem_driver::Device>();
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> clone_directory_result =
      device->CloneServiceDirClientForTests();
  if (clone_directory_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to clone sysmem service directory: "
                   << clone_directory_result.status_string();
  }
  return clone_directory_result;
}

}  // namespace display
