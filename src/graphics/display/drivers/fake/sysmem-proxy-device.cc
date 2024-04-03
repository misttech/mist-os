// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/sysmem-proxy-device.h"

#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fdio/directory.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/channel.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdint>
#include <utility>

#include <ddktl/device.h>
#include <ddktl/unbind-txn.h>

namespace display {

// severity can be ERROR, WARN, INFO, DEBUG, TRACE.  See ddk/debug.h.
//
// Using ## __VA_ARGS__ instead of __VA_OPT__(,) __VA_ARGS__ for now, since
// __VA_OPT__ doesn't seem to be available yet.
#define LOG(severity, fmt, ...) \
  zxlogf(severity, "[%s:%s:%d] " fmt "\n", "display", __func__, __LINE__, ##__VA_ARGS__)

namespace {

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> CloneDirectoryClient(
    fidl::UnownedClientEnd<fuchsia_io::Directory> dir_client) {
  auto clone_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (clone_endpoints.is_error()) {
    LOG(ERROR, "CreateEndpoints failed: %s", clone_endpoints.status_string());
    return zx::error(clone_endpoints.status_value());
  }
  fuchsia_io::Node1CloneRequest clone_request;
  clone_request.flags() = fuchsia_io::OpenFlags::kCloneSameRights;
  clone_request.object() = fidl::ServerEnd<fuchsia_io::Node>(clone_endpoints->server.TakeChannel());
  auto clone_result = fidl::Call(dir_client)->Clone(std::move(clone_request));
  if (clone_result.is_error()) {
    LOG(ERROR, "Clone failed: %s", clone_result.error_value().status_string());
    return zx::error(clone_result.error_value().status());
  }
  return zx::ok(std::move(clone_endpoints->client));
}

void ConnectToSysmemAllocatorV1(fidl::ServerEnd<fuchsia_sysmem::Allocator> request) {
  static constexpr char kServicePath[] = "/svc/fuchsia.sysmem.Allocator";
  LOG(DEBUG, "component::Connect to: %s", kServicePath);
  auto connect_result = component::Connect(std::move(request), kServicePath);
  if (connect_result.is_error()) {
    LOG(ERROR, "component::Connect failed: %s", connect_result.status_string());
  }
}

void ConnectToSysmemAllocatorV2(fidl::ServerEnd<fuchsia_sysmem2::Allocator> request) {
  static constexpr char kServicePath[] = "/svc/fuchsia.sysmem2.Allocator";
  LOG(DEBUG, "component::Connect to: %s", kServicePath);
  auto connect_result = component::Connect(std::move(request), kServicePath);
  if (connect_result.is_error()) {
    LOG(ERROR, "component::Connect failed: %s", connect_result.status_string());
  }
}

fuchsia_hardware_sysmem::Service::InstanceHandler GetSysmemServiceInstanceHandler() {
  return fuchsia_hardware_sysmem::Service::InstanceHandler({
      .sysmem =
          [](fidl::ServerEnd<fuchsia_hardware_sysmem::Sysmem> request) {
            // not requested by tests using fake-display-stack (no registering of external
            // heaps)
            LOG(ERROR, "unexpected request for fuchsia_hardware_sysmem::Sysmem");
            // ~request
          },
      .allocator_v1 =
          [](fidl::ServerEnd<fuchsia_sysmem::Allocator> request) {
            ConnectToSysmemAllocatorV1(std::move(request));
          },
      .allocator_v2 =
          [](fidl::ServerEnd<fuchsia_sysmem2::Allocator> request) {
            ConnectToSysmemAllocatorV2(std::move(request));
          },
  });
}

}  // namespace

SysmemProxyDevice::SysmemProxyDevice(zx_device_t* parent_device,
                                     sysmem_driver::Driver* parent_driver)
    : DdkDeviceType2(parent_device),
      parent_driver_(parent_driver),
      loop_(&kAsyncLoopConfigNeverAttachToThread) {
  ZX_DEBUG_ASSERT(parent_);
  ZX_DEBUG_ASSERT(parent_driver_);
  zx_status_t status = loop_.StartThread("sysmem", &loop_thrd_);
  ZX_ASSERT(status == ZX_OK);
}

void SysmemProxyDevice::ConnectV1(ConnectV1RequestView request,
                                  ConnectV1Completer::Sync& completer) {
  ConnectToSysmemAllocatorV1(std::move(request->allocator_request));
}

void SysmemProxyDevice::ConnectV2(ConnectV2RequestView request,
                                  ConnectV2Completer::Sync& completer) {
  ConnectToSysmemAllocatorV2(std::move(request->allocator_request));
}

void SysmemProxyDevice::SetAuxServiceDirectory(SetAuxServiceDirectoryRequestView request,
                                               SetAuxServiceDirectoryCompleter::Sync& completer) {
  LOG(ERROR, "SysmemProxyDevice::SetAuxServiceDirectory() not supported");
}

zx_status_t SysmemProxyDevice::Bind() {
  // The fuchsia_hardware_sysmem::Service is how drivers connect to sysmem (not DriverConnector,
  // which is for sysmem-connector to connect to sysmem). We implement by forwarding allocator
  // requests to the real sysmem.
  zx::result<fidl::Endpoints<fuchsia_io::Directory>> service_endpoints =
      fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (service_endpoints.is_error()) {
    LOG(ERROR, "fidl::CreateEndpoints failed: %s", service_endpoints.status_string());
    return service_endpoints.error_value();
  }
  // service_outgoing_ can only be created and used on loop_ thread
  std::optional<zx_status_t> status_from_loop;
  libsync::Completion done_creating_outgoing;
  zx_status_t post_status = async::PostTask(
      loop_.dispatcher(), [this, service_server_end = std::move(service_endpoints->server),
                           &done_creating_outgoing, &status_from_loop]() mutable {
        service_outgoing_.emplace(loop_.dispatcher());
        auto add_service_result = service_outgoing_->AddService<fuchsia_hardware_sysmem::Service>(
            GetSysmemServiceInstanceHandler());
        if (add_service_result.is_error()) {
          LOG(ERROR, "AddService failed: %s", add_service_result.status_string());
          status_from_loop = add_service_result.status_value();
          return;
        }
        auto serve_result = service_outgoing_->Serve(std::move(service_server_end));
        if (serve_result.is_error()) {
          LOG(ERROR, "Serve failed: %s", serve_result.status_string());
          status_from_loop = serve_result.status_value();
          return;
        }
        status_from_loop = ZX_OK;
        done_creating_outgoing.Signal();
      });
  if (post_status != ZX_OK) {
    LOG(ERROR, "async::PostTask failed: %s", zx_status_get_string(post_status));
    return post_status;
  }
  done_creating_outgoing.Wait();
  ZX_ASSERT(status_from_loop.has_value());
  if (*status_from_loop != ZX_OK) {
    // loop_ thread already logged what happened
    return *status_from_loop;
  }

  // We stash an outgoing directory SyncClient (in outgoing_dir_client_for_tests_) for later cloning
  // and use by fake-display-stack, at least until MockDdk supports the outgoing dir (as of this
  // comment, doesn't appear to so far).
  auto outgoing_dir_client = std::move(service_endpoints->client);
  auto clone_result = CloneDirectoryClient(outgoing_dir_client);
  if (clone_result.is_error()) {
    LOG(ERROR, "CloneDirectoryClient failed: %s", clone_result.status_string());
    return clone_result.status_value();
  }
  service_client_for_tests_ = std::move(clone_result.value());

  const char* offers_array[] = {
      fuchsia_hardware_sysmem::Service::Name,
  };
  cpp20::span<const char*> offers(offers_array);
  zx_status_t status = DdkAdd(ddk::DeviceAddArgs("sysmem")
                                  .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                                  .set_inspect_vmo(inspector_.DuplicateVmo())
                                  .set_fidl_service_offers(offers)
                                  .set_outgoing_dir(outgoing_dir_client.TakeChannel()));
  if (status != ZX_OK) {
    LOG(ERROR, "Failed to bind device: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx::result<fidl::ClientEnd<fuchsia_io::Directory>>
SysmemProxyDevice::CloneServiceDirClientForTests() {
  return CloneDirectoryClient(service_client_for_tests_);
}

void SysmemProxyDevice::DdkUnbind(ddk::UnbindTxn txn) {
  // Ensure all tasks started before this call finish before shutting down the loop.
  async::PostTask(loop_.dispatcher(), [this]() {
    service_outgoing_.reset();
    loop_.Quit();
  });
  // JoinThreads waits for the Quit() to execute and cause the thread to exit.
  loop_.JoinThreads();
  loop_.Shutdown();
  // After this point the FIDL servers should have been shutdown and all DDK and other protocol
  // methods will error out because posting tasks to the dispatcher fails.
  txn.Reply();
}

}  // namespace display
