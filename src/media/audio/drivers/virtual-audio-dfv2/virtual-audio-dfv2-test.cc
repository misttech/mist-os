// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/virtual-audio-dfv2/virtual-audio-dfv2.h"

#include <lib/driver/testing/cpp/driver_test.h>

#include "src/lib/testing/predicates/status.h"

namespace virtual_audio::test {

class Environment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override { return zx::ok(); }
};

class TestConfig {
 public:
  using DriverType = VirtualAudio;
  using EnvironmentType = Environment;
};

class VirtualAudioTest : public testing::Test {
 public:
  void SetUp() override { ASSERT_OK(driver_test_.StartDriver()); }

  void TearDown() override { ASSERT_OK(driver_test_.StopDriver()); }

 private:
  fdf_testing::BackgroundDriverTest<TestConfig> driver_test_;
};

// Verify that the virtual audio driver can start.
TEST_F(VirtualAudioTest, Start) {}

}  // namespace virtual_audio::test
