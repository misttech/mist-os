// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_DRIVERS_TRANSPORT_DRIVER_V2_CHILD_DRIVER_H_
#define EXAMPLES_DRIVERS_TRANSPORT_DRIVER_V2_CHILD_DRIVER_H_

#include <fidl/fuchsia.examples.gizmo/cpp/driver/wire.h>
#include <lib/driver/component/cpp/driver_base.h>

namespace driver_transport {

class ChildDriverTransportDriver : public fdf::DriverBase {
 public:
  ChildDriverTransportDriver(fdf::DriverStartArgs start_args,
                             fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("driver-transport-child", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

  uint32_t hardware_id() const { return hardware_id_; }
  uint32_t major_version() const { return major_version_; }
  uint32_t minor_version() const { return minor_version_; }

 private:
  zx::result<> QueryParent(fdf::ClientEnd<fuchsia_examples_gizmo::Device> client_end);
  zx::result<> AddChild(std::string_view node_name);

  uint32_t hardware_id_;
  uint32_t major_version_;
  uint32_t minor_version_;

  fidl::WireClient<fuchsia_driver_framework::NodeController> controller_;
};

}  // namespace driver_transport

#endif  // EXAMPLES_DRIVERS_TRANSPORT_DRIVER_V2_CHILD_DRIVER_H_
