// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_DRIVERS_TRANSPORT_DRIVER_V2_PARENT_DRIVER_H_
#define EXAMPLES_DRIVERS_TRANSPORT_DRIVER_V2_PARENT_DRIVER_H_

#include <fidl/fuchsia.examples.gizmo/cpp/driver/wire.h>
#include <lib/driver/component/cpp/driver_base.h>

namespace driver_transport {

class ParentDriverTransportDriver : public fdf::DriverBase,
                                    public fdf::WireServer<fuchsia_examples_gizmo::Device> {
 public:
  ParentDriverTransportDriver(fdf::DriverStartArgs start_args,
                              fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("transport-parent", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

  void GetHardwareId(fdf::Arena& arena, GetHardwareIdCompleter::Sync& completer) override;
  void GetFirmwareVersion(fdf::Arena& arena, GetFirmwareVersionCompleter::Sync& completer) override;

 private:
  fidl::WireClient<fuchsia_driver_framework::NodeController> controller_;

  fdf::ServerBindingGroup<fuchsia_examples_gizmo::Device> server_bindings_;
};

}  // namespace driver_transport

#endif  // EXAMPLES_DRIVERS_TRANSPORT_DRIVER_V2_PARENT_DRIVER_H_
