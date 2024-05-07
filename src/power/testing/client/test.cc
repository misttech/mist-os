// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/power/testing/client/cpp/client.h"

TEST(SystemActivityGovernor, ConnectTest) {
  auto set_result = fidl::Call(*test_client::ConnectSuspendControl())
                        ->SetSuspendStates({{{{
                            fuchsia_hardware_suspend::SuspendState{{.resume_latency = 100}},
                        }}}});

  ASSERT_EQ(true, set_result.is_ok());

  auto governor_result = test_client::ConnectGovernor();
  auto power_elements_result = fidl::Call(*governor_result)->GetPowerElements();
  ASSERT_EQ(true, power_elements_result.is_ok());
}
