// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_VSYNC_MONITOR_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_VSYNC_MONITOR_H_

#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>

#include <atomic>

#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"

namespace display {

// Maintains statistics about Vsync stalls.
class VsyncMonitor {
 public:
  explicit VsyncMonitor(inspect::Node inspect_root);

  VsyncMonitor(const VsyncMonitor&) = delete;
  VsyncMonitor(VsyncMonitor&&) = delete;
  VsyncMonitor& operator=(const VsyncMonitor&) = delete;
  VsyncMonitor& operator=(VsyncMonitor&&) = delete;

  ~VsyncMonitor();

  // Initialization code not suitable for the constructor.
  zx::result<> Initialize();

  void Deinitialize();

  // Called when a display engine driver sends a Vsync event.
  void OnVsync(zx::time vsync_timestamp, ConfigStamp vsync_config_stamp);

 private:
  // Periodically reads `last_vsync_timestamp_` and increments
  // `vsync_stalls_detected_` if no vsync has been observed in a given time
  // period.
  void UpdateStatistics();

  std::atomic<zx::time> last_vsync_timestamp_{};

  inspect::Node inspect_root_;
  inspect::UintProperty last_vsync_ns_property_;
  inspect::UintProperty last_vsync_interval_ns_property_;
  inspect::UintProperty last_vsync_config_stamp_property_;

  // Fields that track how often vsync was detected to have been stalled.
  std::atomic_bool vsync_stalled_ = false;
  inspect::UintProperty vsync_stalls_detected_;
  async::Loop updater_loop_;
  async::TaskClosureMethod<VsyncMonitor, &VsyncMonitor::UpdateStatistics> updater_{this};
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_VSYNC_MONITOR_H_
