// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/fake-display-stack.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <memory>
#include <utility>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/fake/fake-display.h"
#include "src/graphics/display/drivers/fake/sysmem-device-wrapper.h"

namespace display {

FakeDisplayStack::FakeDisplayStack(std::shared_ptr<zx_device> mock_root,
                                   std::unique_ptr<SysmemDeviceWrapper> sysmem,
                                   const fake_display::FakeDisplayDeviceConfig& device_config)
    : mock_root_(mock_root), sysmem_(std::move(sysmem)) {
  pdev_fidl_.SetConfig({
      .use_fake_bti = true,
  });

  auto metadata_result = fidl::Persist(sysmem_metadata_);
  ZX_ASSERT(metadata_result.is_ok());
  std::vector<uint8_t> metadata = std::move(metadata_result.value());
  mock_root_->SetMetadata(fuchsia_hardware_sysmem::wire::kMetadataType, metadata.data(),
                          metadata.size());

  // Protocols for sysmem
  pdev_loop_.StartThread("pdev-server-thread");

  service_loop_.StartThread("outgoing-service-directory-thread");
  libsync::Completion create_outgoing_complete;
  async::TaskClosure create_outgoing_task([&] {
    outgoing_ = component::OutgoingDirectory(service_loop_.dispatcher());
    create_outgoing_complete.Signal();
  });
  ZX_ASSERT(create_outgoing_task.Post(service_loop_.dispatcher()) == ZX_OK);
  create_outgoing_complete.Wait();
  SetUpOutgoingServices();

  fidl::ClientEnd<fuchsia_io::Directory> client = ConnectToOutgoingServiceDirectory();
  mock_root_->AddFidlService(fuchsia_hardware_platform_device::Service::Name, std::move(client));

  if (auto result = sysmem_->Bind(); result != ZX_OK) {
    ZX_PANIC("sysmem_.Bind() return status was not ZX_OK. Error: %s.",
             zx_status_get_string(result));
  }

  // Sysmem device was created as a child under mock_root by the caller.
  sysmem_device_ = mock_root_->GetLatestChild();
  // In mock-ddk, in order for a driver (fake-display) to access a fragment FIDL service
  // (fuchsia.hardware.sysmem/Service), the service outgoing directory must be added to the the
  // parent MockDevice (mock_root_) directly. There's no capability routing between MockDevices.
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> sysmem_service_dir_client_result =
      sysmem_->CloneServiceDirClient();
  if (sysmem_service_dir_client_result.is_error()) {
    ZX_PANIC("CloneServiceDirClient failed: %s", sysmem_service_dir_client_result.status_string());
  }
  fidl::ClientEnd<fuchsia_io::Directory> sysmem_service_dir_client_end =
      std::move(sysmem_service_dir_client_result.value());
  // Fragments for fake-display
  mock_root_->AddFidlService(fuchsia_hardware_sysmem::Service::Name,
                             /*ns=*/std::move(sysmem_service_dir_client_end), "sysmem");
  mock_root_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                             /*ns=*/ConnectToOutgoingServiceDirectory(), "pdev");

  display_ = new fake_display::FakeDisplay(mock_root_.get(), device_config, inspect::Inspector{});
  if (auto status = display_->Bind(); status != ZX_OK) {
    ZX_PANIC("display_->Bind() return status was not ZX_OK. Error: %s.",
             zx_status_get_string(status));
  }
  zx_device_t* mock_display = mock_root_->GetLatestChild();

  // Protocols for display controller.
  mock_display->AddProtocol(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL,
                            display_->display_controller_impl_banjo_protocol()->ops,
                            display_->display_controller_impl_banjo_protocol()->ctx);

  std::unique_ptr<display::Controller> c(new Controller(mock_display));
  // Save a copy for test cases.
  coordinator_controller_ = c.get();
  if (auto status = c->Bind(&c); status != ZX_OK) {
    ZX_PANIC("c->Bind(&c) return status was not ZX_OK. Error: %s.", zx_status_get_string(status));
  }

  auto display_endpoints = fidl::CreateEndpoints<fuchsia_hardware_display::Provider>();
  fidl::BindServer(display_loop_.dispatcher(), std::move(display_endpoints->server),
                   coordinator_controller_);
  display_loop_.StartThread("display-server-thread");
  display_provider_client_ = fidl::WireSyncClient<fuchsia_hardware_display::Provider>(
      std::move(display_endpoints->client));
}

FakeDisplayStack::~FakeDisplayStack() {
  // SyncShutdown() must be called before ~FakeDisplayStack().
  ZX_ASSERT(shutdown_);
}

void FakeDisplayStack::SetUpOutgoingServices() {
  ZX_ASSERT(outgoing_.has_value());
  fuchsia_hardware_platform_device::Service::InstanceHandler platform_device_service_handler(
      {.device = [this](fidl::ServerEnd<fuchsia_hardware_platform_device::Device> request) {
        fidl::BindServer(pdev_loop_.dispatcher(), std::move(request), &pdev_fidl_);
      }});

  libsync::Completion add_services_complete;
  async::TaskClosure add_services_task([&] {
    zx::result<> add_platform_device_service_result =
        outgoing_->AddService<fuchsia_hardware_platform_device::Service>(
            std::move(platform_device_service_handler));
    ZX_ASSERT(add_platform_device_service_result.is_ok());
    add_services_complete.Signal();
  });
  ZX_ASSERT(add_services_task.Post(service_loop_.dispatcher()) == ZX_OK);
  add_services_complete.Wait();
}

fidl::ClientEnd<fuchsia_io::Directory> FakeDisplayStack::ConnectToOutgoingServiceDirectory() {
  auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

  libsync::Completion serve_complete;
  async::TaskClosure serve_task([&] {
    ZX_ASSERT(outgoing_->Serve(std::move(endpoints.server)).is_ok());
    serve_complete.Signal();
  });
  ZX_ASSERT(serve_task.Post(service_loop_.dispatcher()) == ZX_OK);
  serve_complete.Wait();

  return std::move(endpoints.client);
}

const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& FakeDisplayStack::display_client() {
  return display_provider_client_;
}

fidl::ClientEnd<fuchsia_sysmem::Allocator> FakeDisplayStack::ConnectToSysmemAllocatorV1() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_sysmem::Allocator>();
  if (endpoints.is_error()) {
    ZX_PANIC("CreateEndpoints failed - status: %s", endpoints.status_string());
  }
  using ServiceMember = fuchsia_hardware_sysmem::Service::AllocatorV1;
  zx_status_t status = device_connect_fragment_fidl_protocol(
      mock_root_.get(), "sysmem", ServiceMember::ServiceName, ServiceMember::Name,
      endpoints->server.TakeChannel().release());
  if (status != ZX_OK) {
    ZX_PANIC("device_connect_fragment_fidl_protocol failed - status: %s",
             zx_status_get_string(status));
  }
  return std::move(endpoints->client);
}

void FakeDisplayStack::SyncShutdown() {
  if (shutdown_) {
    // SyncShutdown() was already called.
    return;
  }
  shutdown_ = true;

  // Stop serving display loop so that the device can be safely torn down.
  display_loop_.Shutdown();
  display_loop_.JoinThreads();

  coordinator_controller_->DdkAsyncRemove();
  display_->DdkAsyncRemove();
  device_async_remove(sysmem_device_);
  mock_ddk::ReleaseFlaggedDevices(mock_root_.get());

  // All the fake devices are expected to be deleted by `ReleaseFlaggedDevices`.
  display_ = nullptr;
  coordinator_controller_ = nullptr;
  sysmem_device_ = nullptr;

  // Sysmem device is torn down, so there's no driver depending on the pdev
  // server. It's now safe to tear down the pdev loop.
  pdev_loop_.Shutdown();

  // All devices are torn down, so there's no access to the outgoing service
  // directory. It's now safe to remove the outgoing directory and tear down
  // the service loop.
  libsync::Completion shutdown_complete;
  async::TaskClosure shutdown_task([&] {
    outgoing_.reset();
    shutdown_complete.Signal();
  });
  ZX_ASSERT(shutdown_task.Post(service_loop_.dispatcher()) == ZX_OK);
  shutdown_complete.Wait();
  service_loop_.Shutdown();
}

}  // namespace display
