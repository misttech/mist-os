// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/fake-display-stack.h"

#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/fdio/directory.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <memory>
#include <utility>

#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/coordinator/engine-driver-client.h"
#include "src/graphics/display/drivers/fake/fake-display.h"

namespace fake_display {

FakeDisplayStack::FakeDisplayStack(std::unique_ptr<SysmemServiceProvider> sysmem_service_provider,
                                   const FakeDisplayDeviceConfig& device_config)
    : driver_runtime_(mock_ddk::GetDriverRuntime()),
      sysmem_service_provider_(std::move(sysmem_service_provider)) {
  if (!fdf::Logger::HasGlobalInstance()) {
    logger_.emplace();
  }

  fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem_allocator = ConnectToSysmemAllocatorV2();
  display_engine_ = std::make_unique<FakeDisplay>(device_config, std::move(sysmem_allocator),
                                                  inspect::Inspector{});

  zx::result<fdf::SynchronizedDispatcher> create_dispatcher_result =
      fdf::SynchronizedDispatcher::Create(fdf::SynchronizedDispatcher::Options::kAllowSyncCalls,
                                          "display-client-loop",
                                          [this](fdf_dispatcher_t* dispatcher) {
                                            coordinator_client_dispatcher_is_shut_down_.Signal();
                                          });
  if (create_dispatcher_result.is_error()) {
    ZX_PANIC("Failed to create dispatcher: %s", create_dispatcher_result.status_string());
  }
  coordinator_client_dispatcher_ = std::move(create_dispatcher_result).value();

  ddk::DisplayEngineProtocolClient display_engine_client(
      display_engine_->display_engine_banjo_protocol());
  auto engine_driver_client =
      std::make_unique<display_coordinator::EngineDriverClient>(display_engine_client);
  zx::result<std::unique_ptr<display_coordinator::Controller>> create_controller_result =
      display_coordinator::Controller::Create(std::move(engine_driver_client),
                                              coordinator_client_dispatcher_.borrow());
  if (create_controller_result.is_error()) {
    ZX_PANIC("Failed to create display coordinator Controller device: %s",
             create_controller_result.status_string());
  }
  coordinator_controller_ = std::move(create_controller_result).value();

  auto display_endpoints = fidl::CreateEndpoints<fuchsia_hardware_display::Provider>();
  fidl::BindServer(display_loop_.dispatcher(), std::move(display_endpoints->server),
                   coordinator_controller_.get());
  display_loop_.StartThread("display-server-thread");
  display_provider_client_ = fidl::WireSyncClient<fuchsia_hardware_display::Provider>(
      std::move(display_endpoints->client));
}

FakeDisplayStack::~FakeDisplayStack() {
  ZX_ASSERT_MSG(shutdown_, "FakeDisplayStack::SyncShutdown() not called");
}

display_coordinator::Controller* FakeDisplayStack::coordinator_controller() {
  ZX_ASSERT(!shutdown_);
  ZX_ASSERT(coordinator_controller_ != nullptr);
  return coordinator_controller_.get();
}

FakeDisplay& FakeDisplayStack::display_engine() {
  ZX_ASSERT(!shutdown_);
  ZX_ASSERT(display_engine_ != nullptr);
  return *display_engine_;
}

const fidl::WireSyncClient<fuchsia_hardware_display::Provider>&
FakeDisplayStack::display_provider_client() {
  ZX_ASSERT(!shutdown_);
  ZX_ASSERT(display_provider_client_.is_valid());
  return display_provider_client_;
}

fidl::ClientEnd<fuchsia_sysmem2::Allocator> FakeDisplayStack::ConnectToSysmemAllocatorV2() {
  ZX_ASSERT(!shutdown_);

  zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>> connect_allocator_result =
      sysmem_service_provider_->ConnectAllocator2();
  if (connect_allocator_result.is_error()) {
    ZX_PANIC("Failed to connect to sysmem Allocator service: %s",
             connect_allocator_result.status_string());
  }
  return std::move(connect_allocator_result).value();
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

  coordinator_controller_->PrepareStop();

  coordinator_client_dispatcher_.ShutdownAsync();
  coordinator_client_dispatcher_is_shut_down_.Wait();

  coordinator_controller_->Stop();
  coordinator_controller_.reset();
  display_engine_.reset();

  sysmem_service_provider_.reset();
}

}  // namespace fake_display
