// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_DRIVERS_TRANSPORT_BANJO_V2_CHILD_DRIVER_H_
#define EXAMPLES_DRIVERS_TRANSPORT_BANJO_V2_CHILD_DRIVER_H_

#include <fidl/fuchsia.driver.framework/cpp/natural_messaging.h>
#include <fuchsia/examples/gizmo/cpp/banjo.h>
#include <lib/driver/component/cpp/driver_base.h>

namespace banjo_transport {

// Child driver that binds to the ParentBanjoTransportDriver's child node. When the driver starts,
// it connects to the Misc protocol through Banjo transport and queries the Hardware ID and Firmware
// version.
class ChildBanjoTransportDriver : public fdf::DriverBase {
 public:
  ChildBanjoTransportDriver(fdf::DriverStartArgs start_args,
                            fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("banjo-transport-child", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

  zx_status_t QueryParent();

  uint32_t hardware_id() const { return hardware_id_; }
  uint32_t major_version() const { return major_version_; }
  uint32_t minor_version() const { return minor_version_; }

 private:
  ddk::MiscProtocolClient client_;
  fidl::Client<fuchsia_driver_framework::NodeController> controller_;

  // Values queried from the parent driver through Banjo transport. Set in Start().
  uint32_t hardware_id_;
  uint32_t major_version_;
  uint32_t minor_version_;
};

}  // namespace banjo_transport

#endif  // EXAMPLES_DRIVERS_TRANSPORT_BANJO_V2_CHILD_DRIVER_H_
