// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_DRIVERS_TRANSPORT_ZIRCON_V2_PARENT_DRIVER_H_
#define EXAMPLES_DRIVERS_TRANSPORT_ZIRCON_V2_PARENT_DRIVER_H_

#include <fidl/fuchsia.examples.gizmo/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>

namespace zircon_transport {

class ParentZirconTransportDriver : public fdf::DriverBase,
                                    public fidl::WireServer<fuchsia_examples_gizmo::Device> {
 public:
  ParentZirconTransportDriver(fdf::DriverStartArgs start_args,
                              fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("transport-parent", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

  void GetHardwareId(GetHardwareIdCompleter::Sync& completer) override;
  void GetFirmwareVersion(GetFirmwareVersionCompleter::Sync& completer) override;

 private:
  fidl::WireClient<fuchsia_driver_framework::NodeController> controller_;

  fidl::ServerBindingGroup<fuchsia_examples_gizmo::Device> bindings_;
};

}  // namespace zircon_transport

#endif  // EXAMPLES_DRIVERS_TRANSPORT_ZIRCON_V2_PARENT_DRIVER_H_
