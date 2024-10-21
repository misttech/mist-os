// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_TIMEKEEPER_TEST_CLOCK_H_
#define SRC_LIB_TIMEKEEPER_TEST_CLOCK_H_

#include <lib/zx/time.h>

#include "src/lib/timekeeper/clock.h"

namespace timekeeper {

// Implementation of |Clock| that returned a pre-set time.
class TestClock : public Clock {
 public:
  TestClock();
  ~TestClock() override;

  // TODO(https://fxbug.dev/374177211): Remove after clients migrated to other functions.
  template <zx_clock_t kClockId>
  void Set(zx::basic_time<kClockId> time) {
    const zx_time_t time_nanos = time.get();

    current_utc_ = time_nanos;
    current_monotonic_ = time_nanos;
    current_boot_ = time_nanos;
  }

  void SetUtc(time_utc time) { current_utc_ = time.get(); }
  void SetMonotonic(zx::time_monotonic time) { current_monotonic_ = time.get(); }
  void SetBoot(zx::time_boot time) { current_boot_ = time.get(); }

 private:
  zx_status_t GetUtcTime(zx_time_t* time) const override;
  zx_instant_mono_t GetMonotonicTime() const override;
  zx_instant_boot_t GetBootTime() const override;

  zx_time_t current_utc_;
  zx_instant_mono_t current_monotonic_;
  zx_instant_boot_t current_boot_;
};

}  // namespace timekeeper

#endif  // SRC_LIB_TIMEKEEPER_TEST_CLOCK_H_
