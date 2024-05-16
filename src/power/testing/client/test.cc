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

  ASSERT_EQ(true, set_result.is_ok());

  auto governor_result = client.ConnectGovernor();
  auto power_elements_result = fidl::Call(*governor_result)->GetPowerElements();
  ASSERT_EQ(true, power_elements_result.is_ok());

  auto suspend_states = fidl::Call(*client.ConnectSuspender())->GetSuspendStates();
  ASSERT_EQ(true, suspend_states.is_ok());
}
