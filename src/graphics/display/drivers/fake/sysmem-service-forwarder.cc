// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/sysmem-service-forwarder.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/sync/cpp/completion.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>

#include <fbl/alloc_checker.h>

namespace display {

// static
zx::result<std::unique_ptr<SysmemServiceForwarder>> SysmemServiceForwarder::Create() {
  fbl::AllocChecker alloc_checker;
  auto component_sysmem_service_forwarder =
      fbl::make_unique_checked<SysmemServiceForwarder>(&alloc_checker);
  if (!alloc_checker.check()) {
    FX_LOGS(ERROR) << "Failed to allocate memory for ComponentSysmemServiceForwarder";
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> initialize_result = component_sysmem_service_forwarder->Initialize();
  if (initialize_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to initialize ComponentSysmemServiceForwarder: "
                   << initialize_result.status_string();
    return initialize_result.take_error();
  }

  return zx::ok(std::move(component_sysmem_service_forwarder));
}

SysmemServiceForwarder::SysmemServiceForwarder() : loop_(&kAsyncLoopConfigNeverAttachToThread) {
  zx_status_t start_thread_status = loop_.StartThread("service-loop");
  ZX_ASSERT(start_thread_status == ZX_OK);
}

SysmemServiceForwarder::~SysmemServiceForwarder() {
  // Ensure all tasks started before this call finish before shutting down the loop.
  async::PostTask(loop_.dispatcher(), [this]() {
    outgoing_.reset();
    loop_.Quit();
  });
  // Waits for the Quit() to execute and cause the thread to exit.
  loop_.JoinThreads();
  loop_.Shutdown();
}

zx::result<> SysmemServiceForwarder::Initialize() {
  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> incoming_service_root_result =
      component::OpenServiceRoot();
  if (incoming_service_root_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to open incoming service directory: "
                   << incoming_service_root_result.status_string();
    return incoming_service_root_result.take_error();
  }
  component_incoming_root_ = std::move(incoming_service_root_result).value();

  libsync::Completion outgoing_init_completed;
  async::PostTask(loop_.dispatcher(), [&] {
    outgoing_ = component::OutgoingDirectory(loop_.dispatcher());
    outgoing_init_completed.Signal();
  });
  outgoing_init_completed.Wait();

  libsync::Completion add_service_completed;
  zx::result<> add_service_result = zx::error(ZX_ERR_INTERNAL);
  async::PostTask(loop_.dispatcher(), [&] {
    add_service_result = outgoing_->AddService<fuchsia_hardware_sysmem::Service>(
        CreateSysmemServiceInstanceHandler());
    add_service_completed.Signal();
  });
  add_service_completed.Wait();
  if (add_service_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to add sysmem service to the outgoing service directory: "
                   << add_service_result.status_string();
    return add_service_result.take_error();
  }

  auto [outgoing_service_root_client, outgoing_service_root_server] =
      fidl::Endpoints<fuchsia_io::Directory>::Create();
  libsync::Completion serve_completed;
  zx::result<> serve_result = zx::error(ZX_ERR_INTERNAL);
  async::PostTask(loop_.dispatcher(), [&, outgoing_service_root_server =
                                              std::move(outgoing_service_root_server)]() mutable {
    serve_result = outgoing_->Serve(std::move(outgoing_service_root_server));
    serve_completed.Signal();
  });
  serve_completed.Wait();
  if (serve_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing service directory: "
                   << serve_result.status_string();
    return serve_result.take_error();
  }
  forwarder_outgoing_root_ = std::move(outgoing_service_root_client);

  return zx::ok();
}

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> SysmemServiceForwarder::GetOutgoingDirectory() {
  auto [cloned_client, cloned_server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  fuchsia_io::Node1CloneRequest clone_request{{
      .flags = fuchsia_io::OpenFlags::kCloneSameRights,
      .object = fidl::ServerEnd<fuchsia_io::Node>(cloned_server.TakeChannel()),
  }};
  fit::result<fidl::OneWayError> clone_result =
      fidl::Call(forwarder_outgoing_root_)->Clone(std::move(clone_request));
  if (clone_result.is_error()) {
    FX_LOGS(ERROR) << "Failed to clone the outgoing root directory: "
                   << clone_result.error_value().FormatDescription();
    return zx::error(clone_result.error_value().status());
  }
  return zx::ok(std::move(cloned_client));
}

fuchsia_hardware_sysmem::Service::InstanceHandler
SysmemServiceForwarder::CreateSysmemServiceInstanceHandler() const {
  return fuchsia_hardware_sysmem::Service::InstanceHandler{{
      .sysmem =
          [](fidl::ServerEnd<fuchsia_hardware_sysmem::Sysmem> sysmem_server) {
            ZX_ASSERT_MSG(false, "fuchsia.hardware.sysmem/Sysmem protocol not supported");
          },
      .allocator_v1 =
          [this](fidl::ServerEnd<fuchsia_sysmem::Allocator> allocator_server) {
            zx::result<> connect_result = component::ConnectAt<fuchsia_sysmem::Allocator>(
                component_incoming_root_, std::move(allocator_server));
            ZX_ASSERT_MSG(connect_result.is_ok(), "Failed to connect to AllocatorV1: %s",
                          connect_result.status_string());
          },
      .allocator_v2 =
          [this](fidl::ServerEnd<fuchsia_sysmem2::Allocator> allocator_server) {
            zx::result<> connect_result = component::ConnectAt<fuchsia_sysmem2::Allocator>(
                component_incoming_root_, std::move(allocator_server));
            ZX_ASSERT_MSG(connect_result.is_ok(), "Failed to connect to AllocatorV2: %s",
                          connect_result.status_string());
          },
  }};
}

}  // namespace display
