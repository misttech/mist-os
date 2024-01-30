// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.inspect/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/component/cpp/service.h>

namespace inspect {
#if __Fuchsia_API_level__ >= 16
ComponentInspector::ComponentInspector(async_dispatcher_t* dispatcher, PublishOptions opts)
    : inspector_(std::move(opts.inspector)) {
  auto client_end = opts.client_end.has_value()
                        ? std::move(*opts.client_end)
                        : component::Connect<fuchsia_inspect::InspectSink>().value();
  auto endpoints = fidl::CreateEndpoints<fuchsia_inspect::Tree>();
  TreeServer::StartSelfManagedServer(inspector_, std::move(opts.tree_handler_settings), dispatcher,
                                     std::move(endpoints->server));

  fidl::Client client(std::move(client_end), dispatcher);
  auto result =
      client->Publish({{.tree = std::move(endpoints->client), .name = std::move(opts.tree_name)}});
  ZX_ASSERT(result.is_ok());
}

void PublishVmo(async_dispatcher_t* dispatcher, zx::vmo vmo, VmoOptions opts) {
  auto client_end = opts.client_end.has_value()
                        ? std::move(*opts.client_end)
                        : component::Connect<fuchsia_inspect::InspectSink>().value();
  auto endpoints = fidl::CreateEndpoints<fuchsia_inspect::Tree>();
  TreeServer::StartSelfManagedServer(std::move(vmo), {}, dispatcher, std::move(endpoints->server));

  fidl::Client client(std::move(client_end), dispatcher);
  auto result =
      client->Publish({{.tree = std::move(endpoints->client), .name = std::move(opts.tree_name)}});
  ZX_ASSERT(result.is_ok());
}
#else
ComponentInspector::ComponentInspector(component::OutgoingDirectory& outgoing_directory,
                                       async_dispatcher_t* dispatcher, Inspector inspector,
                                       TreeHandlerSettings settings)
    : inspector_(inspector) {
  auto status = outgoing_directory.AddUnmanagedProtocolAt<fuchsia_inspect::Tree>(
      "diagnostics", [dispatcher, inspector = std::move(inspector), settings = std::move(settings)](
                         fidl::ServerEnd<fuchsia_inspect::Tree> server_end) {
        TreeServer::StartSelfManagedServer(std::move(inspector), std::move(settings), dispatcher,
                                           std::move(server_end));
      });

  ZX_ASSERT(status.is_ok());
}
#endif  // __Fuchsia_API_level__

NodeHealth& ComponentInspector::Health() {
  if (!component_health_) {
    component_health_ = std::make_unique<NodeHealth>(&inspector().GetRoot());
  }
  return *component_health_;
}
}  // namespace inspect
