// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/driver/v2/parent-driver.h"

#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace driver_transport {

zx::result<> ParentDriverTransportDriver::Start() {
  // Publish `fuchsia.examples.gizmo.Service` to the outgoing directory.
  fuchsia_examples_gizmo::Service::InstanceHandler handler({
      .device = server_bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                               fidl::kIgnoreBindingClosure),
  });
  zx::result result = outgoing()->AddService<fuchsia_examples_gizmo::Service>(std::move(handler));
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add service", KV("status", result.status_string()));
    return result.take_error();
  }

  // Add a child with a `fuchsia.examples.gizmo.Service` offer.
  zx::result child_result =
      AddChild("driver_transport_child", {}, {fdf::MakeOffer2<fuchsia_examples_gizmo::Service>()});
  if (child_result.is_error()) {
    return child_result.take_error();
  }

  controller_.Bind(std::move(child_result.value()), dispatcher());
  return zx::ok();
}

void ParentDriverTransportDriver::GetHardwareId(fdf::Arena& arena,
                                                GetHardwareIdCompleter::Sync& completer) {
  completer.buffer(arena).ReplySuccess(0x1234ABCD);
}

void ParentDriverTransportDriver::GetFirmwareVersion(fdf::Arena& arena,
                                                     GetFirmwareVersionCompleter::Sync& completer) {
  completer.buffer(arena).ReplySuccess(0x0, 0x1);
}

}  // namespace driver_transport

FUCHSIA_DRIVER_EXPORT(driver_transport::ParentDriverTransportDriver);
