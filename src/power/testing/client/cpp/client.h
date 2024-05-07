// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_TESTING_CLIENT_CPP_CLIENT_H_
#define SRC_POWER_TESTING_CLIENT_CPP_CLIENT_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.suspend/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/fidl.h>
#include <fidl/test.suspendcontrol/cpp/fidl.h>

namespace test_client {

zx::result<fidl::ClientEnd<fuchsia_power_system::ActivityGovernor>> ConnectGovernor();

zx::result<fidl::ClientEnd<fuchsia_power_suspend::Stats>> ConnectStats();

zx::result<fidl::ClientEnd<test_sagcontrol::State>> ConnectFakeSAGControl();

zx::result<fidl::ClientEnd<fuchsia_power_broker::Topology>> ConnectPowerBrokerTopology();

zx::result<fidl::ClientEnd<test_suspendcontrol::Device>> ConnectSuspendControl();

zx::result<fidl::ClientEnd<fuchsia_hardware_suspend::Suspender>> ConnectSuspender();

}  // namespace test_client

#endif  // SRC_POWER_TESTING_CLIENT_CPP_CLIENT_H_
