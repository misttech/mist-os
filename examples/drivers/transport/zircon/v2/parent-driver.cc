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
  zx::result child_result =
      AddChild("zircon_transport_child", {}, {fdf::MakeOffer2<fuchsia_examples_gizmo::Service>()});
  if (child_result.is_error()) {
    return child_result.take_error();
  }
  controller_.Bind(std::move(child_result.value()), dispatcher());

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
