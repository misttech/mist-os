// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_POWER_MANAGER_H_
#define SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_POWER_MANAGER_H_

#include <lib/fit/thread_safety.h>
#include <lib/magma/platform/platform_semaphore.h>
#include <lib/magma/util/short_macros.h>
#include <lib/magma_service/util/register_io.h>

#include <chrono>
#include <deque>
#include <mutex>

#include "mali_register_io.h"

// This class generally lives on the device thread.
class PowerManager {
 public:
  class Owner {
   public:
    virtual mali::RegisterIo* register_io() = 0;
  };

  explicit PowerManager(Owner* owner);

  // Called on the device thread or the initial driver thread.
  void EnableCores(uint64_t shader_bitmask);

  // Called on the GPU interrupt thread.
  void ReceivedPowerInterrupt();

  uint64_t l2_ready_status() const {
    std::lock_guard<std::mutex> lock(ready_status_mutex_);
    return l2_ready_status_;
  }

  // This is called whenever the GPU starts or stops processing work.
  void UpdateGpuActive(bool active);

  // Retrieves information on what fraction of time in the recent past (last
  // 100 ms or so) the GPU was actively processing commands.
  void GetGpuActiveInfo(std::chrono::steady_clock::duration* total_time_out,
                        std::chrono::steady_clock::duration* active_time_out);
  bool GetTotalTime(uint32_t* buffer_out);

  void DisableL2();
  void DisableShaders();
  bool WaitForL2Disable();
  bool WaitForShaderDisable();
  bool WaitForShaderReady();

 private:
  friend class TestMsdArmDevice;
  friend class TestPowerManager;

  struct TimePeriod {
    std::chrono::steady_clock::time_point end_time;
    std::chrono::steady_clock::duration total_time;
    std::chrono::steady_clock::duration active_time;
  };

  mali::RegisterIo* register_io() { return owner_->register_io(); }
  void UpdateReadyStatus();
  // Called to update timekeeping and possible update the gpu activity info.
  void UpdateGpuActiveLocked(bool active) FIT_REQUIRES(active_time_mutex_);
  std::deque<TimePeriod>& time_periods() { return time_periods_; }

  Owner* owner_;

  mutable std::mutex ready_status_mutex_;
  FIT_GUARDED(ready_status_mutex_) uint64_t tiler_ready_status_ = 0;
  FIT_GUARDED(ready_status_mutex_) uint64_t l2_ready_status_ = 0;

  std::unique_ptr<magma::PlatformSemaphore> power_state_semaphore_;

  std::mutex active_time_mutex_;
  FIT_GUARDED(active_time_mutex_) std::deque<TimePeriod> time_periods_;
  // |gpu_active_| is true if the GPU is currently processing work.
  FIT_GUARDED(active_time_mutex_) bool gpu_active_ = false;
  FIT_GUARDED(active_time_mutex_) std::chrono::steady_clock::time_point last_check_time_;
  FIT_GUARDED(active_time_mutex_) std::chrono::steady_clock::time_point last_trace_time_;

  FIT_GUARDED(active_time_mutex_) uint64_t total_active_time_ = 0u;
};

#endif  // SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_SRC_POWER_MANAGER_H_
