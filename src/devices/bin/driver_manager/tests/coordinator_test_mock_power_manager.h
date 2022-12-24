// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_TESTS_COORDINATOR_TEST_MOCK_POWER_MANAGER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_TESTS_COORDINATOR_TEST_MOCK_POWER_MANAGER_H_

#include <fidl/fuchsia.power.manager/cpp/wire.h>
#include <lib/zx/channel.h>

class MockPowerManager : public fidl::WireServer<fuchsia_power_manager::DriverManagerRegistration> {
  void Register(RegisterRequestView request, RegisterCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_TESTS_COORDINATOR_TEST_MOCK_POWER_MANAGER_H_
