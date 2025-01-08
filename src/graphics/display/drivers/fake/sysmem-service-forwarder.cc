// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/sysmem-service-forwarder.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/sync/cpp/completion.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>

namespace display {

// static
zx::result<std::unique_ptr<SysmemServiceForwarder>> SysmemServiceForwarder::Create() {
  auto sysmem_service_forwarder = std::make_unique<SysmemServiceForwarder>();

  zx::result<> initialize_result = sysmem_service_forwarder->Initialize();
  if (initialize_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to initialize SysmemServiceForwarder: "
                   << initialize_result.status_string();
    return initialize_result.take_error();
  }

  return zx::ok(std::move(sysmem_service_forwarder));
}

SysmemServiceForwarder::SysmemServiceForwarder() = default;
SysmemServiceForwarder::~SysmemServiceForwarder() = default;

zx::result<> SysmemServiceForwarder::Initialize() {
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> incoming_service_root_result =
      component::OpenServiceRoot();
  if (incoming_service_root_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to open incoming service directory: "
                   << incoming_service_root_result.status_string();
    return incoming_service_root_result.take_error();
  }
  component_incoming_root_ = std::move(incoming_service_root_result).value();

  return zx::ok();
}

zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>>
SysmemServiceForwarder::ConnectAllocator2() {
  auto [client_end, server_end] = fidl::Endpoints<fuchsia_sysmem2::Allocator>::Create();
  zx::result<> connect_result =
      component::ConnectAt(component_incoming_root_, std::move(server_end));
  if (connect_result.is_error()) {
    return connect_result.take_error();
  }
  return zx::ok(std::move(client_end));
}

zx::result<fidl::ClientEnd<fuchsia_hardware_sysmem::Sysmem>>
SysmemServiceForwarder::ConnectHardwareSysmem() {
  ZX_ASSERT_MSG(false, "fuchsia.hardware.sysmem/Sysmem protocol not supported");
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

}  // namespace display
