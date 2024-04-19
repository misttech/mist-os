// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BUS_DRIVERS_PLATFORM_TEST_TEST_CHILD_DEVICE_H_
#define SRC_DEVICES_BUS_DRIVERS_PLATFORM_TEST_TEST_CHILD_DEVICE_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>

namespace fake_child_device {

class FakeChild : public fdf::DriverBase {
 public:
  FakeChild(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("fake-child", std::move(start_args), std::move(dispatcher)) {}

  ~FakeChild() override = default;

  zx::result<> Start() override;

 private:
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  fidl::ClientEnd<fuchsia_power_broker::RequiredLevel> required_level_;
  fidl::ClientEnd<fuchsia_power_broker::CurrentLevel> current_level_;
  fidl::ClientEnd<fuchsia_power_broker::Lessor> lessor_;
};
}  // namespace fake_child_device

#endif /* SRC_DEVICES_BUS_DRIVERS_PLATFORM_TEST_TEST_CHILD_DEVICE_H_ */
