// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_TEST_DEVICE_HOST_H_
#define SRC_MEDIA_AUDIO_DRIVERS_TEST_DEVICE_HOST_H_

#include <fuchsia/virtualaudio/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>

#include "src/lib/fsl/io/device_watcher.h"
#include "src/media/audio/drivers/test/test_base.h"

namespace media::audio::drivers::test {

class DeviceHost {
 public:
  DeviceHost();
  ~DeviceHost();

  // Post a task to our thread to detect and add all devices, so that driver testing can begin.
  void AddDevices(bool devfs_only, bool no_virtual_audio);

  // Create testcase instances for each device entry, based on the passed-in configuration.
  void RegisterTests(bool expect_audio_core_not_connected, bool enable_position_tests);

  // Testing is complete. Clean up our virtual audio devices and shut down our loop.
  zx_status_t QuitDeviceLoop();

 private:
  // Use exaggerated timeouts to eliminate flakes on loaded CQ instances running debug builds.
  static constexpr zx::duration kDeviceWatcherTimeout = zx::sec(1);
  static constexpr zx::duration kAddAllDevicesTimeout = zx::sec(3);

  // Detect devfs-based audio devices, optionally adding device entries for a2dp and virtual_audio.
  void DetectDevices(bool devfs_only, bool no_virtual_audio);

  // Optionally called during DetectDevices. Create virtual_audio instances (one for input, one for
  // output) using the default configurations settings (which should always pass all tests).
  void AddVirtualDevices();

  std::set<DeviceEntry>& device_entries() { return device_entries_; }
  std::vector<std::unique_ptr<fsl::DeviceWatcher>>& device_watchers() { return device_watchers_; }

  async::Loop device_loop_;
  std::set<DeviceEntry> device_entries_;
  std::vector<std::unique_ptr<fsl::DeviceWatcher>> device_watchers_;

  fuchsia::virtualaudio::ControlSyncPtr controller_ = nullptr;
  fuchsia::virtualaudio::DevicePtr output_device_ = nullptr;
  fuchsia::virtualaudio::DevicePtr input_device_ = nullptr;
  bool shutting_down_ = false;
};

}  // namespace media::audio::drivers::test

#endif  // SRC_MEDIA_AUDIO_DRIVERS_TEST_DEVICE_HOST_H_
