// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/power/testing/client/cpp/client.h"

#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/incoming/cpp/service.h>

namespace test_client {

constexpr const char kComponentName[] = "power_framework_test_realm";
constexpr const char kPowerFrameworkTestRealmUrl[] = "power-framework#meta/power-framework.cm";

PowerTestingClient::PowerTestingClient()
    : root_(component_testing::RealmBuilder::Create()
                .AddChild(kComponentName, kPowerFrameworkTestRealmUrl)
                .AddRoute(component_testing::Route{
                    .capabilities =
                        {
                            component_testing::Protocol{"fuchsia.power.broker.Topology"},
                            component_testing::Protocol{"fuchsia.power.suspend.Stats"},
                            component_testing::Protocol{"fuchsia.power.system.ActivityGovernor"},
                            component_testing::Protocol{"test.sagcontrol.State"},
                            component_testing::Protocol{"test.suspendcontrol.Device"},
                            component_testing::Service{"fuchsia.hardware.suspend.SuspendService"},
                        },
                    .source = component_testing::ChildRef{kComponentName},
                    .targets = {component_testing::ParentRef()}})
                .Build()) {}

zx::result<fidl::ClientEnd<fuchsia_power_system::ActivityGovernor>>
PowerTestingClient::ConnectGovernor() {
  return root_.component().Connect<fuchsia_power_system::ActivityGovernor>();
}
zx::result<fidl::ClientEnd<fuchsia_power_suspend::Stats>> PowerTestingClient::ConnectStats() {
  return root_.component().Connect<fuchsia_power_suspend::Stats>();
}

zx::result<fidl::ClientEnd<test_sagcontrol::State>> PowerTestingClient::ConnectFakeSAGControl() {
  return root_.component().Connect<test_sagcontrol::State>();
}

zx::result<fidl::ClientEnd<fuchsia_power_broker::Topology>>
PowerTestingClient::ConnectPowerBrokerTopology() {
  return root_.component().Connect<fuchsia_power_broker::Topology>();
}

zx::result<fidl::ClientEnd<test_suspendcontrol::Device>>
PowerTestingClient::ConnectSuspendControl() {
  return root_.component().Connect<test_suspendcontrol::Device>();
}

zx::result<fidl::ClientEnd<fuchsia_hardware_suspend::Suspender>>
PowerTestingClient::ConnectSuspender() {
  fidl::UnownedClientEnd<fuchsia_io::Directory> svc(
      root_.component().exposed().unowned_channel()->get());
  return component::ConnectAtMember<fuchsia_hardware_suspend::SuspendService::Suspender>(svc);
}

}  // namespace test_client
