// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_SYSTEM_ACTIVITY_GOVERNOR_INTEGRATION_TESTING_FAKE_SUSPEND_FAKE_SUSPEND_H_
#define SRC_POWER_SYSTEM_ACTIVITY_GOVERNOR_INTEGRATION_TESTING_FAKE_SUSPEND_FAKE_SUSPEND_H_

#include <fidl/fuchsia.hardware.suspend/cpp/fidl.h>
#include <fidl/test.suspendcontrol/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/devfs/cpp/connector.h>

#include <memory>
#include <vector>

#include "src/power/testing/fake-suspend/control_server.h"
#include "src/power/testing/fake-suspend/device_server.h"

namespace fake_suspend {

class Driver : public fdf::DriverBase {
 public:
  Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  zx::result<> Start() override;

 private:
  void ServeSuspender(fidl::ServerEnd<fuchsia_hardware_suspend::Suspender> server);
  void ServeSuspendControl(fidl::ServerEnd<test_suspendcontrol::Device> server);

  template <typename T>
  zx::result<fidl::ClientEnd<fuchsia_driver_framework::Node>*> AddChild(
      fidl::ClientEnd<fuchsia_driver_framework::Node>* parent, std::string_view node_name,
      std::string_view class_name, driver_devfs::Connector<T>& devfs_connector);

  driver_devfs::Connector<fuchsia_hardware_suspend::Suspender> suspender_connector_;
  driver_devfs::Connector<test_suspendcontrol::Device> control_connector_;

  std::vector<fidl::ClientEnd<fuchsia_driver_framework::Node>> nodes_;
  std::vector<fidl::WireSyncClient<fuchsia_driver_framework::NodeController>> controllers_;

  std::shared_ptr<std::vector<fuchsia_hardware_suspend::SuspendState>> suspend_states_;
  std::shared_ptr<DeviceServer> device_server_;
  std::shared_ptr<ControlServer> control_server_;
};

}  // namespace fake_suspend

#endif  // SRC_POWER_SYSTEM_ACTIVITY_GOVERNOR_INTEGRATION_TESTING_FAKE_SUSPEND_FAKE_SUSPEND_H_
