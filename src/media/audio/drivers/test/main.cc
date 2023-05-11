// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/cpp/task.h>
#include <lib/syslog/cpp/log_settings.h>
#include <zircon/types.h>

#include <gtest/gtest.h>

#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/test/test_settings.h"
#include "src/media/audio/drivers/test/device_host.h"

int main(int argc, char** argv) {
  auto command_line = fxl::CommandLineFromArgcArgv(argc, argv);
  if (!fxl::SetTestSettings(command_line)) {
    return EXIT_FAILURE;
  }

  fuchsia_logging::SetTags({"audio_driver_tests"});
  testing::InitGoogleTest(&argc, argv);

  // --devfs-only: Only test devices detected in devfs; don't add/test Bluetooth audio a2dp output.
  bool devfs_only = command_line.HasOption("devfs-only");

  // --no-virtual: Unless disabled via this flag, two virtual_audio instances (1 input, 1 output)
  //   are automatically enabled (with default settings) and tested.
  bool no_virtual_audio = command_line.HasOption("no-virtual");

  // --admin: Validate commands that require the privileged channel, such as SetFormat.
  //   Otherwise, omit AdminTest cases if a device/driver is exposed in the device tree.
  //   TODO(fxbug.dev/93428): Enable tests if we see audio_core isn't connected to drivers.
  bool expect_audio_core_not_connected = command_line.HasOption("admin");

  // --run-position-tests: Include audio position test cases (requires realtime capable system).
  bool enable_position_tests = command_line.HasOption("run-position-tests");

  media::audio::drivers::test::DeviceHost device_host{};
  // The default configuration for each of these four booleans is FALSE.
  device_host.AddDevices(devfs_only, no_virtual_audio);
  device_host.RegisterTests(expect_audio_core_not_connected, enable_position_tests);

  auto ret = RUN_ALL_TESTS();

  if (device_host.QuitDeviceLoop() != ZX_OK) {
    FX_LOGS(ERROR) << "DeviceHost::QuitDeviceLoop timed out";
    return EXIT_FAILURE;
  }

  return ret;
}
