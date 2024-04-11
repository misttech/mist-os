// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/coordinator-getter/client.h"

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/fdio/directory.h>
#include <lib/fpromise/bridge.h>
#include <lib/fpromise/promise.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <lib/zx/result.h>
#include <zircon/status.h>

#include <memory>

namespace display {

namespace {

using ProviderClientEnd = fidl::ClientEnd<fuchsia_hardware_display::Provider>;

zx::result<ProviderClientEnd> GetProvider() {
  auto [client, server] = fidl::Endpoints<fuchsia_hardware_display::Provider>::Create();

  constexpr const char* kServicePath =
      fidl::DiscoverableProtocolDefaultPath<fuchsia_hardware_display::Provider>;
  zx_status_t status = fdio_service_connect(kServicePath, server.TakeChannel().release());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to connect to " << kServicePath << ": ";
    return zx::error(status);
  }
  return zx::ok(std::move(client));
}

fpromise::promise<CoordinatorClientEnd, zx_status_t> GetCoordinatorFromProvider(
    async_dispatcher_t* dispatcher, ProviderClientEnd& provider_client) {
  auto [coordinator_client, coordinator_server] =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();

  fpromise::bridge<void, zx_status_t> bridge;
  std::shared_ptr completer =
      std::make_shared<decltype(bridge.completer)>(std::move(bridge.completer));

  // fidl::Client requires that it must be bound on the dispatcher thread.
  // So this has to be dispatched as an async task running on `dispatcher`.
  async::PostTask(dispatcher, [completer, dispatcher, provider_client = std::move(provider_client),
                               coordinator_server = std::move(coordinator_server)]() mutable {
    fidl::Client<fuchsia_hardware_display::Provider> client(std::move(provider_client), dispatcher);
    // The FIDL Client is retained in the `Then` handler, to keep the
    // connection open until the response is received.
    client->OpenCoordinatorForPrimary({{.coordinator = std::move(coordinator_server)}})
        .Then([completer, client = std::move(client)](
                  fidl::Result<fuchsia_hardware_display::Provider::OpenCoordinatorForPrimary>&
                      result) {
          if (result.is_error()) {
            zx_status_t status = result.error_value().status();
            FX_PLOGS(ERROR, status) << "OpenCoordinatorForPrimary FIDL error: ";
            completer->complete_error(status);
            return;
          }
          fuchsia_hardware_display::ProviderOpenCoordinatorForPrimaryResponse& response =
              result.value();
          if (response.s() != ZX_OK) {
            FX_PLOGS(ERROR, response.s()) << "OpenCoordinatorForPrimary responded with error: ";
            completer->complete_error(response.s());
            return;
          }
          completer->complete_ok();
        });
  });

  return bridge.consumer.promise().and_then(
      [coordinator_client = std::move(coordinator_client)]() mutable {
        return fpromise::ok(std::move(coordinator_client));
      });
}

}  // namespace

fpromise::promise<CoordinatorClientEnd, zx_status_t> GetCoordinator(
    async_dispatcher_t* dispatcher) {
  TRACE_DURATION("gfx", "GetCoordinator");
  zx::result client_end = GetProvider();
  if (client_end.is_error()) {
    return fpromise::make_result_promise<CoordinatorClientEnd, zx_status_t>(
        fpromise::error(client_end.error_value()));
  }
  return GetCoordinatorFromProvider(dispatcher, *client_end);
}

fpromise::promise<CoordinatorClientEnd, zx_status_t> GetCoordinator() {
  return GetCoordinator(async_get_default_dispatcher());
}

}  // namespace display
