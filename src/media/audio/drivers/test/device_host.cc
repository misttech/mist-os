// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/test/device_host.h"

#include <fuchsia/virtualaudio/cpp/fidl.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/fdio/directory.h>
#include <lib/sync/cpp/completion.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <zircon/system/public/zircon/compiler.h>

#include <string>
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
                                        bool expect_audio_core_not_connected);
extern void RegisterPositionTestsForDevice(const DeviceEntry& device_entry,
                                           bool expect_audio_core_not_connected,
                                           bool enable_position_tests);

static const struct {
  const char* path;
  DriverType driver_type;
} kAudioDevNodes[] = {
    {.path = "/dev/class/audio-input", .driver_type = DriverType::StreamConfigInput},
    {.path = "/dev/class/audio-output", .driver_type = DriverType::StreamConfigOutput},
    {.path = "/dev/class/dai", .driver_type = DriverType::Dai},
};

// Our thread and dispatcher must exist during the entirety of test execution; create it now.
DeviceHost::DeviceHost() : device_loop_(async::Loop(&kAsyncLoopConfigNeverAttachToThread)) {
  device_loop_.StartThread("AddVadAndDetectDevices");
}
DeviceHost::~DeviceHost() { QuitDeviceLoop(); }

// Post a task to our thread to detect and add all devices, so that testing can begin.
void DeviceHost::AddDevices(bool devfs_only, bool no_virtual_audio) {
  libsync::Completion done;
  async::PostTask(device_loop_.dispatcher(), [this, &done, devfs_only, no_virtual_audio]() {
    DetectDevices(devfs_only, no_virtual_audio);
    done.Signal();
  });
  ASSERT_EQ(done.Wait(kAddAllDevicesTimeout), ZX_OK)
      << "Deadlock in FidlThread::Create while creating 'AddVadAndDetectDevices' thread";
}

// Set up DeviceWatchers to detect input and output devices, run the initial enumeration of devfs,
// add virtual_audio instances (optionally) and an a2dp instance (optionally).
void DeviceHost::DetectDevices(bool devfs_only, bool no_virtual_audio) {
  DeviceType dev_type = DeviceType::BuiltIn;
  bool initial_enumeration_done;

  // Set up the device watchers. If any fail, automatically stop monitoring all device sources.
  // First, we add any preexisting ("built-in") devices.
  for (const auto& devnode : kAudioDevNodes) {
    initial_enumeration_done = false;
    auto watcher = fsl::DeviceWatcher::CreateWithIdleCallback(
        devnode.path,
        [driver_type = devnode.driver_type, &dev_type, &dev_entries = device_entries()](
            const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename) {
          FX_LOGS(TRACE) << "dir handle " << dir.channel().get() << " for '" << filename << "' ("
                         << dev_type << " " << driver_type << ")";
          dev_entries.insert({dir, filename, driver_type, dev_type});
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
        << devnode.driver_type;
  }

  // Then, if enabled, enable virtual_audio instances and wait for their detection.
  // By reusing the watchers we've already configured, we detect each device only once.
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
    device_entries().insert({{}, "A2DP", DriverType::StreamConfigOutput, DeviceType::A2DP});
  }
}

// Optionally called during DetectDevices. Create virtual_audio instances -- one for input, one for
// output -- using the default configurations settings (which should pass all tests).
void DeviceHost::AddVirtualDevices() {
  const std::string kControlNodePath =
      fxl::Concatenate({"/dev/", fuchsia::virtualaudio::CONTROL_NODE_NAME});
  zx_status_t status = fdio_service_connect(kControlNodePath.c_str(),
                                            controller_.NewRequest().TakeChannel().release());
  ASSERT_EQ(status, ZX_OK) << "fdio_service_connect failed";

  uint32_t num_inputs(-1), num_outputs(-1);
  status = controller_->GetNumDevices(&num_inputs, &num_outputs);
  ASSERT_EQ(status, ZX_OK) << "GetNumDevices failed";
  ASSERT_TRUE(controller_.is_bound()) << "virtualaudio::Control did not stay bound";
  ASSERT_EQ(num_inputs, 0u) << num_inputs << " virtual_audio inputs already exist (should be 0)";
  ASSERT_EQ(num_outputs, 0u) << num_outputs << " virtual_audio outputs already exist (should be 0)";

  fuchsia::virtualaudio::Configuration input_config;
  fuchsia::virtualaudio::Control_AddDevice_Result input_result;
  input_config.set_is_input(true);
  status = controller_->AddDevice(
      std::move(input_config), input_device_.NewRequest(device_loop_.dispatcher()), &input_result);
  ASSERT_EQ(status, ZX_OK) << "virtualaudio::Control::AddDevice (input) failed";
  ASSERT_FALSE(input_result.is_err()) << "Failed to add input device: " << input_result.err();
  input_device_.set_error_handler(
      [](zx_status_t error) { FAIL() << "virtualaudio::Device (input) disconnected: " << error; });

  fuchsia::virtualaudio::Configuration output_config;
  fuchsia::virtualaudio::Control_AddDevice_Result output_result;
  status =
      controller_->AddDevice(std::move(output_config),
                             output_device_.NewRequest(device_loop_.dispatcher()), &output_result);
  ASSERT_EQ(status, ZX_OK) << "virtualaudio::Control::AddOutput failed";
  ASSERT_FALSE(output_result.is_err()) << "Failed to add output device: " << output_result.err();
  output_device_.set_error_handler(
      [](zx_status_t error) { FAIL() << "virtualaudio::Device (output) disconnected: " << error; });
}

// Create testcase instances for each device entry.
void DeviceHost::RegisterTests(bool expect_audio_core_not_connected, bool enable_position_tests) {
  for (auto& device_entry : device_entries()) {
    RegisterBasicTestsForDevice(device_entry);
    RegisterAdminTestsForDevice(device_entry, expect_audio_core_not_connected);
    RegisterPositionTestsForDevice(device_entry, expect_audio_core_not_connected,
                                   enable_position_tests);
  }
}

// Testing is complete. Clean up our virtual audio devices and shut down our loop.
zx_status_t DeviceHost::QuitDeviceLoop() {
  if (shutting_down_) {
    return ZX_OK;
  }
  shutting_down_ = true;

  if (device_loop_.GetState() == ASYNC_LOOP_SHUTDOWN) {
    return ZX_OK;
  }

  libsync::Completion done;
  async::PostTask(device_loop_.dispatcher(), [this, &done]() {
    input_device_.set_error_handler(nullptr);
    output_device_.set_error_handler(nullptr);

    if (controller_.is_bound()) {
      zx_status_t status = controller_->RemoveAll();
      ASSERT_EQ(status, ZX_OK) << "RemoveAll failed";

      uint32_t input_count(-1), output_count(-1);
      do {
        status = controller_->GetNumDevices(&input_count, &output_count);
        ASSERT_EQ(status, ZX_OK) << "GetNumDevices failed";
      } while (input_count != 0 || output_count != 0);
    }

    device_loop_.RunUntilIdle();
    done.Signal();
  });

  zx_status_t status = done.Wait(zx::sec(10));
  device_loop_.Shutdown();

  return status;
}

}  // namespace media::audio::drivers::test
