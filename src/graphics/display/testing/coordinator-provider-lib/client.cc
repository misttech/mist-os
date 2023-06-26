// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/testing/coordinator-provider-lib/client.h"

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/fdio/directory.h>
#include <lib/fpromise/bridge.h>
#include <lib/fpromise/promise.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <zircon/status.h>

#include <memory>

namespace display {

namespace {

using ProviderClientEnd = fidl::ClientEnd<fuchsia_hardware_display::Provider>;

class IoNodeEventHandler : public fidl::AsyncEventHandler<fuchsia_io::Node> {
 public:
  using OnOpenCallback = fit::function<void(fidl::Event<fuchsia_io::Node::OnOpen>& event)>;
  using OnFidlErrorCallback = fit::function<void(fidl::UnbindInfo error)>;

  IoNodeEventHandler() = default;
  void OnOpen(fidl::Event<fuchsia_io::Node::OnOpen>& event) override { on_open_(event); }
  void on_fidl_error(fidl::UnbindInfo error) override { on_fidl_error_(error); }

  // Sets the OnOpen() callback to `on_open` and returns the old callback.
  OnOpenCallback SetOnOpen(OnOpenCallback on_open) {
    on_open_.swap(on_open);
    return on_open;
  }

  // Sets the OnFidlError() callback to `on_fidl_error` and returns the old
  // callback.
  OnFidlErrorCallback SetOnFidlError(OnFidlErrorCallback on_fidl_error) {
    on_fidl_error_.swap(on_fidl_error);
    return on_fidl_error;
  }

  // Clears event handler callbacks and returns the old callbacks.
  std::pair<OnOpenCallback, OnFidlErrorCallback> TakeCallbacksAndReset() {
    auto on_open = SetOnOpen({});
    auto on_fidl_error = SetOnFidlError({});
    return std::make_pair(std::move(on_open), std::move(on_fidl_error));
  }

 private:
  OnOpenCallback on_open_;
  OnFidlErrorCallback on_fidl_error_;
};

// Connects to the dispatcher
fpromise::promise<ProviderClientEnd, zx_status_t> GetProvider(async_dispatcher_t* dispatcher) {
  zx::result<fidl::Endpoints<fuchsia_io::Node>> endpoints =
      fidl::CreateEndpoints<fuchsia_io::Node>();
  if (endpoints.is_error()) {
    FX_PLOGS(ERROR, endpoints.status_value()) << "Failed to create fuchsia.io.Node endpoints: ";
    return fpromise::make_result_promise<ProviderClientEnd, zx_status_t>(
        fpromise::error(endpoints.error_value()));
  }
  auto& [node_client, node_server] = endpoints.value();

  constexpr const char* kServicePath =
      fidl::DiscoverableProtocolDefaultPath<fuchsia_hardware_display::Provider>;
  zx_status_t status =
      fdio_open(kServicePath, static_cast<uint32_t>(fuchsia_io::wire::OpenFlags::kDescribe),
                node_server.TakeChannel().release());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to connect to " << kServicePath;
    return fpromise::make_result_promise<ProviderClientEnd, zx_status_t>(fpromise::error(status));
  }

  fpromise::bridge<ProviderClientEnd, zx_status_t> bridge;
  std::shared_ptr completer =
      std::make_shared<decltype(bridge.completer)>(std::move(bridge.completer));

  std::unique_ptr event_handler = std::make_unique<IoNodeEventHandler>();

  // fidl::Client is required since we need to get the io.Node client endpoint
  // (and repurpose it as a display.Provider client) after the endpoint gets
  // unbound from the Client. This adds thread safety requirements for methods
  // of `client`.
  std::unique_ptr client = std::make_unique<fidl::Client<fuchsia_io::Node>>();

  IoNodeEventHandler* event_handler_raw = event_handler.get();
  fidl::Client<fuchsia_io::Node>* client_raw = client.get();

  // The FIDL async client (with its client bindings) and the event handler
  // bound to the client are moved to the `OnOpen()` callback closure.
  //
  // Closures are removed from the event handler only on success open (OnOpen
  // event) or FIDL errors, so the FIDL connection will be valid until then.
  event_handler_raw->SetOnOpen([completer, client = std::move(client),
                                event_handler = std::move(event_handler)](auto&) mutable {
    auto unbind_result = client->UnbindMaybeGetEndpoint();
    if (unbind_result.is_error()) {
      zx_status_t status = unbind_result.error_value().status();
      FX_PLOGS(ERROR, status) << "Failed to unbind the channel from async Node client.";
      completer->complete_error(status);
    } else {
      // When a Node is opened using `kDescribe` mode, once the the open is
      // successful, the target protocol (here it's fuchsia.hardware.display.
      // Provider) can be used exclusively on the channel.
      completer->complete_ok(ProviderClientEnd(unbind_result.value().TakeChannel()));
    }
    // Move the callbacks to this closure so that they'll be destroyed when
    // this callback finishes. Destroyal will always occur on `dispatcher`
    // thread.
    auto callbacks = event_handler->TakeCallbacksAndReset();
  });
  event_handler_raw->SetOnFidlError([completer, event_handler_raw](fidl::UnbindInfo error) {
    FX_PLOGS(ERROR, error.status()) << "FIDL client unbound:";
    completer->complete_error(error.status());
    // Move the callbacks to this closure so that they'll be destroyed when
    // this callback finishes. Destroyal will always occur on `dispatcher`
    // thread.
    auto callbacks = event_handler_raw->TakeCallbacksAndReset();
  });

  // The FIDL client and event handler have been moved to the OnOpen() callback
  // which will be only modified after the FIDL client is bound to a channel.
  // So it is safe to use the raw pointers to fidl::Client and
  // IoNodeEventHandler here to bind the channel to the client.
  //
  // fidl::Client requires that it must be bound on the dispatcher thread.
  // So this has to be dispatched as an async task running on `dispatcher`.
  async::PostTask(dispatcher, [client_raw, event_handler_raw, dispatcher,
                               node_client = std::move(node_client)]() mutable {
    client_raw->Bind(std::move(node_client), dispatcher, event_handler_raw);
  });

  return bridge.consumer.promise();
}

fpromise::promise<CoordinatorClientEnd, zx_status_t> GetCoordinatorFromProvider(
    async_dispatcher_t* dispatcher, ProviderClientEnd& provider_client) {
  zx::result<fidl::Endpoints<fuchsia_hardware_display::Coordinator>> coordinator_endpoints_result =
      fidl::CreateEndpoints<fuchsia_hardware_display::Coordinator>();
  if (!coordinator_endpoints_result.is_ok()) {
    FX_PLOGS(ERROR, coordinator_endpoints_result.status_value())
        << "Failed to create fuchsia.hardware.display.Coordinator endpoints: ";
    return fpromise::make_result_promise<CoordinatorClientEnd, zx_status_t>(
        fpromise::error(coordinator_endpoints_result.status_value()));
  }
  auto& [coordinator_client, coordinator_server] = coordinator_endpoints_result.value();

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
  return GetProvider(dispatcher).and_then([dispatcher](ProviderClientEnd& client_end) {
    return GetCoordinatorFromProvider(dispatcher, client_end);
  });
}

fpromise::promise<CoordinatorClientEnd, zx_status_t> GetCoordinator() {
  return GetCoordinator(async_get_default_dispatcher());
}

}  // namespace display
