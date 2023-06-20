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

  // --no-virtual: Don't automatically create and test virtual_audio instances for StreamConfig
  // Dai and Composite (using default settings). When this flag is enabled, any _preexisting_
  // virtual_audio instances are allowed and tested like any other physical device.
  bool no_virtual_audio = command_line.HasOption("no-virtual");

  // --admin: Validate commands that require exclusive access, such as SetFormat.
  //   Otherwise, omit AdminTest cases if a device/driver is exposed in the device tree.
  //   TODO(fxbug.dev/93428): Enable AdminTests if no service is already connected to the driver.
  bool expect_audio_core_not_connected = command_line.HasOption("admin");

  // --run-position-tests: Include audio position test cases (requires realtime capable system).
  bool enable_position_tests = command_line.HasOption("run-position-tests");

  media::audio::drivers::test::DeviceHost device_host;
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
