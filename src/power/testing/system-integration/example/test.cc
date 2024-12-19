// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.development/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <gtest/gtest.h>

TEST(ExamplePowerSystemIntegrationTest, MyTest) {
  auto mgr = component::Connect<fuchsia_driver_development::Manager>();
  EXPECT_EQ(true, mgr.is_ok());
}
