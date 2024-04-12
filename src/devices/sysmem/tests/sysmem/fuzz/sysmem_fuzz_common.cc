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
    loop_.Shutdown();
    sysmem_.DdkAsyncRemove();
    mock_ddk::ReleaseFlaggedDevices(root_.get());
    sysmem_.ResetThreadCheckerForTesting();
    ZX_ASSERT(sysmem_.logical_buffer_collections().size() == 0);
    initialized_ = false;
  }
}
bool MockDdkSysmem::Init() {
  if (initialized_) {
    fprintf(stderr, "MockDdkSysmem already initialized.\n");
    fflush(stderr);
    return false;
  }
  // Avoid wasting fuzzer time outputting logs.
  mock_ddk::SetMinLogSeverity(FX_LOG_FATAL);
  // Pick a platform where AFBC textures will be used. Also add a protected pool to test code that
  // handles that specially (though protected allocations will always fail because the pool is never
  // marked ready).
  static const fuchsia_hardware_sysmem::Metadata metadata = [] {
    fuchsia_hardware_sysmem::Metadata metadata;
    metadata.vid() = PDEV_VID_AMLOGIC;
    metadata.pid() = PDEV_PID_AMLOGIC_S905D2;
    metadata.protected_memory_size() = 1024 * 1024;
    metadata.contiguous_memory_size() = 0;
    return metadata;
  }();

  auto persist_result = fidl::Persist(metadata);
  ZX_ASSERT(persist_result.is_ok());
  auto& persisted_metadata = persist_result.value();

  root_->SetMetadata(fuchsia_hardware_sysmem::wire::kMetadataType, persisted_metadata.data(),
                     persisted_metadata.size());

  sysmem_.set_settings(sysmem_driver::Settings{.max_allocation_size = 256 * 1024});

  loop_.StartThread();
  return initialized_;
}

zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>> MockDdkSysmem::Connect() {
  auto driver_endpoints = fidl::CreateEndpoints<fuchsia_hardware_sysmem::DriverConnector>();
  if (driver_endpoints.is_error()) {
    return zx::error(driver_endpoints.status_value());
  }

  fidl::BindServer(loop_.dispatcher(), std::move(driver_endpoints->server), &sysmem_);

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
