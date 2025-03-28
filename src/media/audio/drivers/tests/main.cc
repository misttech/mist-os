// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/cpp/task.h>
#include <lib/syslog/cpp/log_settings.h>
#include <zircon/types.h>

#include <gtest/gtest.h>

#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/test/test_settings.h"
#include "src/media/audio/drivers/tests/device_host.h"

namespace {

constexpr std::string kHelpOption = "help";
constexpr std::string kDevFsOnlyOption = "devfs-only";
constexpr std::string kNoVirtualAudioOption = "no-virtual";
constexpr std::string kRunAdminTestsOption = "admin";
constexpr std::string kRunPositionTestsOption = "position";

void usage(const std::string& prog_name) {
  printf("\n");
  printf("Usage: %s [--option] [--option] [...]\n\n", prog_name.c_str());

  printf("Test audio drivers via direct FIDL calls\n\n");

  printf("This test suite validates audio drivers. By default, it tests those that it finds in\n");
  printf("devfs, all virtual_audio device types, and the helper library used by Bluetooth to\n");
  printf("implement audio drivers.\n\n");

  printf("By default, this suite assumes that an audio service is already running and connected\n");
  printf("to audio drivers. For this reason, for devfs drivers it does not execute test cases\n");
  printf("that require exclusive (or 'admin') access -- unless explicitly directed to do so.\n\n");

  printf("One subset of 'admin' tests is 'position' tests. The user should only execute these\n");
  printf("test cases on systems with guaranteed real-time response, such as a non-emulated\n");
  printf("environment running a release build with standard optimizations.\n\n");

  printf("Valid options:\n\n");

  printf(
      "  --%s\t\t   Do not test the Bluetooth helper library; only test drivers found in devfs\n",
      kDevFsOnlyOption.c_str());
  printf("  --%s\t\t   Do not instantiate+test 'virtual_audio' instances\n",
         kNoVirtualAudioOption.c_str());
  printf("  \t\t\t   Note: this switch allows any ALREADY-RUNNING 'virtual_audio' instances to\n");
  printf("  \t\t\t   be detected in devfs and tested, like drivers for real physical devices\n");
  printf("  --%s\t\t   For devfs drivers, also run tests that require exclusive driver access\n",
         kRunAdminTestsOption.c_str());
  printf("  \t\t\t   (requires that neither AudioCore nor AudioDeviceRegistry are running)\n");
  printf("  --%s\t   Also run position tests (assumes '--%s')\n", kRunPositionTestsOption.c_str(),
         kRunAdminTestsOption.c_str());
  printf("\n");
}

}  // namespace

int main(int argc, char** argv) {
  auto command_line = fxl::CommandLineFromArgcArgv(argc, argv);
  if (!fxl::SetTestSettings(command_line)) {
    return EXIT_FAILURE;
  }

  if (command_line.HasOption(kHelpOption)) {
    std::string app_name, sub_str;
    std::stringstream ss(command_line.argv0());
    while (std::getline(ss, sub_str, '/')) {
      app_name = sub_str;
    }
    usage(app_name);
    return EXIT_SUCCESS;
  }

  fuchsia_logging::LogSettingsBuilder builder;
  builder.WithTags({"audio_driver_tests"}).BuildAndInitialize();
  testing::InitGoogleTest(&argc, argv);

  // --devfs-only: Only test devices detected in devfs; don't add/test Bluetooth audio a2dp output.
  bool devfs_only = command_line.HasOption(kDevFsOnlyOption);

  // --no-virtual: Don't automatically create and test virtual_audio instances for StreamConfig
  //   Dai and Composite (using default settings). When this flag is enabled, any _preexisting_
  //   virtual_audio instances are allowed and tested like any other physical device.
  bool no_virtual_audio = command_line.HasOption(kNoVirtualAudioOption);

  // --admin: Validate commands that require exclusive access, such as SetFormat.
  //   Otherwise, omit AdminTest cases if a device/driver is exposed in the device tree.
  //   TODO(https://fxbug.dev/42175212): Enable AdminTests if no service is already connected to
  //   the driver.
  bool expect_audio_svcs_not_connected = command_line.HasOption(kRunAdminTestsOption);

  // --position: Include audio position test cases (requires realtime capable system).
  bool enable_position_tests = command_line.HasOption(kRunPositionTestsOption);

  media::audio::drivers::test::DeviceHost device_host;
  // The default configuration for each of these four booleans is FALSE.
  device_host.AddDevices(devfs_only, no_virtual_audio);
  device_host.RegisterTests(expect_audio_svcs_not_connected, enable_position_tests);

  auto ret = RUN_ALL_TESTS();

  if (device_host.QuitDeviceLoop() != ZX_OK) {
    FX_LOGS(ERROR) << "DeviceHost::QuitDeviceLoop timed out";
    return EXIT_FAILURE;
  }

  return ret;
}
