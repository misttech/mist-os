// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/power/testing/client/cpp/client.h"

TEST(SystemActivityGovernor, ConnectTest) {
  test_client::PowerTestingClient client;
  auto set_result = fidl::Call(*client.ConnectSuspendControl())
                        ->SetSuspendStates({{{{
                            fuchsia_hardware_suspend::SuspendState{{.resume_latency = 100}},
                        }}}});

  ASSERT_EQ(true, set_result.is_ok()) << set_result.error_value();

  auto power_elements_result = fidl::Call(*client.ConnectGovernor())->GetPowerElements();
  ASSERT_EQ(true, power_elements_result.is_ok()) << power_elements_result.error_value();

  auto suspend_states = fidl::Call(*client.ConnectSuspender())->GetSuspendStates();
  ASSERT_EQ(true, suspend_states.is_ok()) << suspend_states.error_value();
}
