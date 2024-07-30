// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_DRIVERS_TRANSPORT_DRIVER_V2_CHILD_DRIVER_H_
#define EXAMPLES_DRIVERS_TRANSPORT_DRIVER_V2_CHILD_DRIVER_H_

#include <fidl/fuchsia.hardware.i2cimpl/cpp/driver/wire.h>
#include <lib/driver/component/cpp/driver_base.h>

namespace driver_transport {

// A driver that binds to the child node added by ParentTransportDriver. This driver
// connects to the fuchsia.hardware.i2cimpl and use it to interact with the parent.
class ChildTransportDriver : public fdf::DriverBase {
 public:
  ChildTransportDriver(fdf::DriverStartArgs start_args,
                       fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("driver-transport-child", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

  size_t max_transfer_size() const { return max_transfer_size_; }

 private:
  zx::result<> QueryParent(fdf::ClientEnd<fuchsia_hardware_i2cimpl::Device> client_end);

  size_t max_transfer_size_;

  fidl::WireClient<fuchsia_driver_framework::NodeController> controller_;
};

}  // namespace driver_transport

#endif  // EXAMPLES_DRIVERS_TRANSPORT_DRIVER_V2_CHILD_DRIVER_H_
