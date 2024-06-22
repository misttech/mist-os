// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sysmem_fuzz_common.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include "log_rtn.h"

MockDdkSysmem::~MockDdkSysmem() {
  if (initialized_) {
    sysmem_->DdkAsyncRemove();
    sysmem_ = nullptr;
    // fence the async remove
    mock_ddk::ReleaseFlaggedDevices(root_.get());

    libsync::Completion done;
    ZX_ASSERT(ZX_OK == async::PostTask(loop_.dispatcher(), [this, &done]() mutable {
                outgoing_.reset();
                done.Signal();
              }));
    done.Wait();

    loop_.Shutdown();

    initialized_ = false;
  }
}
bool MockDdkSysmem::Init() {
  if (initialized_) {
    fprintf(stderr, "MockDdkSysmem already initialized.\n");
    fflush(stderr);
    return false;
  }
  // The rest of this method asserts success.
  initialized_ = true;

  // Avoid wasting fuzzer time outputting logs.
  mock_ddk::SetMinLogSeverity(FX_LOG_FATAL);

  zx_status_t set_config_status =
      pdev_.SetConfig(fake_pdev::FakePDevFidl::Config{.use_fake_bti = true});
  ZX_ASSERT(set_config_status == ZX_OK);

  loop_.StartThread();

  auto directory_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ZX_ASSERT(directory_endpoints.is_ok());

  libsync::Completion done;
  ZX_ASSERT(ZX_OK ==
            async::PostTask(loop_.dispatcher(),
                            [this, directory_server_end = std::move(directory_endpoints->server),
                             &done]() mutable {
                              outgoing_.emplace(loop_.dispatcher());
                              auto add_service_result =
                                  outgoing_->AddService<fuchsia_hardware_platform_device::Service>(
                                      pdev_.GetInstanceHandler(loop_.dispatcher()));
                              ZX_ASSERT(add_service_result.is_ok());
                              ZX_ASSERT(outgoing_->Serve(std::move(directory_server_end)).is_ok());
                              done.Signal();
                            }));
  done.Wait();

  root_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                        std::move(directory_endpoints->client));

  auto sysmem = std::make_unique<sysmem_driver::Device>(root_.get(), &sysmem_ctx_);
  ZX_ASSERT(ZX_OK == sysmem->Bind());
  // The ptr is now owned by MockDevice root_. We set the sysmem_ ptr back to nullptr when
  // DdkRelease happens.
  sysmem_ = sysmem.release();

  sysmem_->set_settings(sysmem_driver::Settings{.max_allocation_size = 256 * 1024});

  return true;
}

zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>> MockDdkSysmem::Connect() {
  auto driver_endpoints = fidl::CreateEndpoints<fuchsia_hardware_sysmem::DriverConnector>();
  if (driver_endpoints.is_error()) {
    return zx::error(driver_endpoints.status_value());
  }

  fidl::BindServer(loop_.dispatcher(), std::move(driver_endpoints->server), sysmem_);

  auto [allocator_client_end, allocator_server_end] =
      fidl::Endpoints<fuchsia_sysmem::Allocator>::Create();

  fidl::WireSyncClient<fuchsia_hardware_sysmem::DriverConnector> driver_client(
      std::move(driver_endpoints->client));
  fidl::Status result = driver_client->ConnectV1(std::move(allocator_server_end));
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok(std::move(allocator_client_end));
}
