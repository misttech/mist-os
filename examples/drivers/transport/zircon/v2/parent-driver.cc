// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/zircon/v2/parent-driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace zircon_transport {

zx::result<> ParentZirconTransportDriver::Start() {
  fuchsia_examples_gizmo::Service::InstanceHandler handler({
      .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                        fidl::kIgnoreBindingClosure),
  });
  auto result = outgoing()->AddService<fuchsia_examples_gizmo::Service>(std::move(handler));
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add protocol", KV("status", result.status_string()));
    return result.take_error();
  }

  // Add a child with a `fuchsia.examples.gizmo.Service` offer.
  fidl::Arena arena;
  fidl::VectorView<fuchsia_driver_framework::wire::Offer> offers(arena, 1);
  offers[0] = fdf::MakeOffer2<fuchsia_examples_gizmo::Service>(arena);

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, "zircon_transport_child")
                  .offers2(offers)
                  .Build();

  // Create endpoints of the `NodeController` for the node.
  auto endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (endpoints.is_error()) {
    FDF_SLOG(ERROR, "Failed to create endpoint", KV("status", endpoints.status_string()));
    return zx::error(endpoints.status_value());
  }

  auto child_result = fidl::WireCall(node())->AddChild(args, std::move(endpoints->server), {});
  if (!child_result.ok()) {
    FDF_SLOG(ERROR, "Failed to add child", KV("status", child_result.status_string()));
    return zx::error(child_result.status());
  }
  controller_.Bind(std::move(endpoints->client), dispatcher());

  return zx::ok();
}

void ParentZirconTransportDriver::GetHardwareId(GetHardwareIdCompleter::Sync& completer) {
  completer.ReplySuccess(0x1234ABCD);
}

void ParentZirconTransportDriver::GetFirmwareVersion(GetFirmwareVersionCompleter::Sync& completer) {
  completer.ReplySuccess(0x0, 0x1);
}

}  // namespace zircon_transport

FUCHSIA_DRIVER_EXPORT(zircon_transport::ParentZirconTransportDriver);
