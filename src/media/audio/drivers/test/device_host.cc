// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/test/device_host.h"

#include <fuchsia/virtualaudio/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/fdio/directory.h>
#include <lib/sync/cpp/completion.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>

#include <thread>

#include <gtest/gtest.h>

#include "src/lib/fsl/io/device_watcher.h"
#include "src/lib/fxl/strings/concatenate.h"
#include "src/media/audio/drivers/test/test_base.h"

namespace media::audio::drivers::test {

// TODO(fxbug.dev/65580): Previous implementation used value-parameterized testing. Consider
// reverting to this, moving AddDevices to a function called at static initialization time. If we
// cannot access cmdline flags at that time, this would force us to always register admin tests,
// skipping them at runtime based on the cmdline flag.

extern void RegisterBasicTestsForDevice(const DeviceEntry& device_entry);
extern void RegisterAdminTestsForDevice(const DeviceEntry& device_entry,
                                        bool expect_audio_core_connected);
extern void RegisterPositionTestsForDevice(const DeviceEntry& device_entry,
                                           bool expect_audio_core_connected,
                                           bool enable_position_tests);

static const struct {
  const char* path;
  DeviceDirection device_direction;
} kAudioDevNodes[] = {
    {.path = "/dev/class/audio-input", .device_direction = DeviceDirection::Input},
    {.path = "/dev/class/audio-output", .device_direction = DeviceDirection::Output},
};

// Our thread and dispatcher must exist during the entirety of test execution; create it now.
DeviceHost::DeviceHost() : device_loop_(async::Loop(&kAsyncLoopConfigNeverAttachToThread)) {
  device_loop_.StartThread("AddVadAndDetectDevices");
}

// Post a task to our thread to detect and add all devices, so that testing can begin.
void DeviceHost::AddDevices(bool devfs_only, bool no_virtual_audio) {
  libsync::Completion done;
  async::PostTask(device_loop_.dispatcher(), [this, &done, devfs_only, no_virtual_audio]() {
    DetectDevices(devfs_only, no_virtual_audio);
    done.Signal();
  });
  ASSERT_EQ(done.Wait(zx::sec(10)), ZX_OK)
      << "Deadlock in FidlThread::Create while creating 'AddVadAndDetectDevices' thread";
}

// Set up DeviceWatchers to detect input and output devices, run the initial enumeration of devfs,
// add virtual_audio instances (optionally) and an a2dp instance (optionally).
void DeviceHost::DetectDevices(bool devfs_only, bool no_virtual_audio) {
  constexpr zx::duration kDeviceWatcherTimeout = zx::sec(2);

  DeviceType dev_type = DeviceType::BuiltIn;
  bool initial_enumeration_done;

  // Set up the device watchers. If any fail, automatically stop monitoring all device sources.
  // First, we add any preexisting ("built-in") devices.
  for (const auto& devnode : kAudioDevNodes) {
    initial_enumeration_done = false;
    auto watcher = fsl::DeviceWatcher::CreateWithIdleCallback(
        devnode.path,
        [dev_direction = devnode.device_direction, &dev_type, &dev_entries = device_entries()](
            const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename) {
          FX_LOGS(TRACE) << "dir handle " << dir.channel().get() << " for '" << filename << "' ("
                         << dev_type << " " << dev_direction << ")";
          dev_entries.insert({dir, filename, dev_direction, dev_type});
        },
        [&initial_enumeration_done]() { initial_enumeration_done = true; },
        device_loop_.dispatcher());

    if (watcher == nullptr) {
      ASSERT_FALSE(watcher == nullptr)
          << "AudioDriver::TestBase failed creating DeviceWatcher for '" << devnode.path << "'.";
    }
    device_watchers().emplace_back(std::move(watcher));

    auto deadline = zx::clock::get_monotonic() + kDeviceWatcherTimeout;
    while (!initial_enumeration_done && zx::clock::get_monotonic() < deadline) {
      device_loop_.RunUntilIdle();
    }
    ASSERT_TRUE(initial_enumeration_done)
        << "DeviceWatcher did not finish initial enumeration, for " << dev_type << "/"
        << devnode.device_direction;
  }

  // Then, unless explicitly excluded, enable virtual_audio instances and wait for their
  // detection. By reusing the watchers we've already configured, we detect each device only once.
  if (!no_virtual_audio) {
    auto device_count = device_entries().size();
    dev_type = DeviceType::Virtual;
    AddVirtualDevices();
    auto deadline = zx::clock::get_monotonic() + kDeviceWatcherTimeout;
    while (device_entries().size() < device_count + 2 && zx::clock::get_monotonic() < deadline) {
      device_loop_.RunUntilIdle();
    }
    ASSERT_GE(device_entries().size(), device_count + 2)
        << "DeviceWatcher timed out, for " << dev_type << " devices";
  }

  // And finally, unless expressly excluded, add a device entry for the a2dp-source output device
  // driver, to validate admin functions even if AudioCore has connected to "real" audio drivers.
  if (!devfs_only) {
    device_entries().insert({{}, "A2DP", DeviceDirection::Output, DeviceType::A2DP});
  }
}

// Optionally called during DetectDevices. Create virtual_audio instances -- one for input, one for
// output -- using the default configurations settings (which should pass all tests).
void DeviceHost::AddVirtualDevices() {
  const std::string kControlNodePath =
      fxl::Concatenate({"/dev/", fuchsia::virtualaudio::CONTROL_NODE_NAME});
  zx_status_t status = fdio_service_connect(
      kControlNodePath.c_str(),
      controller_.NewRequest(device_loop_.dispatcher()).TakeChannel().release());
  controller_.set_error_handler(
      [](zx_status_t error) { FAIL() << "virtualaudio::Controller disconnected: " << error; });
  ASSERT_EQ(status, ZX_OK) << "fdio_service_connect failed: " << status;

  bool out_added = false;
  fuchsia::virtualaudio::Configuration output_config;
  bool in_added = false;
  controller_->AddOutput(std::move(output_config),
                         output_device_.NewRequest(device_loop_.dispatcher()),
                         [&out_added](fuchsia::virtualaudio::Control_AddOutput_Result result) {
                           out_added = true;
                           if (result.is_err()) {
                             FAIL() << "Failed to add output device: " << result.err();
                           }
                         });
  output_device_.set_error_handler(
      [](zx_status_t error) { FAIL() << "virtualaudio::Device (output) disconnected: " << error; });

  if (status == ZX_OK) {
    fuchsia::virtualaudio::Configuration input_config;
    controller_->AddInput(std::move(input_config),
                          input_device_.NewRequest(device_loop_.dispatcher()),
                          [&in_added](fuchsia::virtualaudio::Control_AddInput_Result result) {
                            in_added = true;
                            if (result.is_err()) {
                              FAIL() << "Failed to add input device: " << result.err();
                            }
                          });
    input_device_.set_error_handler([](zx_status_t error) {
      FAIL() << "virtualaudio::Device (input) disconnected: " << error;
    });
  }

  auto deadline = zx::clock::get_monotonic() + zx::sec(1);
  while (!(out_added && in_added) && zx::clock::get_monotonic() < deadline) {
    device_loop_.RunUntilIdle();
  }
  EXPECT_TRUE(out_added) << "Could not add virtual_audio output";
  EXPECT_TRUE(in_added) << "Could not add virtual_audio input";
}

// Create testcase instances for each device entry.
void DeviceHost::RegisterTests(bool expect_audio_core_connected, bool enable_position_tests) {
  for (auto& device_entry : device_entries()) {
    RegisterBasicTestsForDevice(device_entry);
    RegisterAdminTestsForDevice(device_entry, expect_audio_core_connected);
    RegisterPositionTestsForDevice(device_entry, expect_audio_core_connected,
                                   enable_position_tests);
  }
}

// Testing is complete. Clean up our virtual audio devices and shut down our loop.
zx_status_t DeviceHost::QuitDeviceLoop() {
  libsync::Completion done;
  output_device_.Unbind();
  input_device_.Unbind();
  controller_.Unbind();

  async::PostTask(device_loop_.dispatcher(), [&loop = device_loop_, &done]() {
    loop.RunUntilIdle();
    loop.Quit();
    done.Signal();
  });
  return done.Wait(zx::sec(10));
}

}  // namespace media::audio::drivers::test
