// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/power/testing/client/cpp/client.h"

#include <lib/component/incoming/cpp/protocol.h>

namespace test_client {
zx::result<fidl::ClientEnd<fuchsia_power_system::ActivityGovernor>> ConnectGovernor() {
  return component::Connect<fuchsia_power_system::ActivityGovernor>();
}
zx::result<fidl::ClientEnd<fuchsia_power_suspend::Stats>> ConnectStats() {
  return component::Connect<fuchsia_power_suspend::Stats>();
}

zx::result<fidl::ClientEnd<test_sagcontrol::State>> ConnectFakeSAGControl() {
  return component::Connect<test_sagcontrol::State>();
}

zx::result<fidl::ClientEnd<fuchsia_power_broker::Topology>> ConnectPowerBrokerTopology() {
  return component::Connect<fuchsia_power_broker::Topology>();
}

zx::result<fidl::ClientEnd<test_suspendcontrol::Device>> ConnectSuspendControl() {
  return component::Connect<test_suspendcontrol::Device>(
      "/dev/class/test/instance/device_protocol");
}

zx::result<fidl::ClientEnd<fuchsia_hardware_suspend::Suspender>> ConnectSuspender() {
  return component::Connect<fuchsia_hardware_suspend::Suspender>("/dev/class/suspend/instance");
}
}  // namespace test_client
