// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_UTIL_H_
#define THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_UTIL_H_

#include <lib/zx/time.h>

#include "pw_chrono/system_clock.h"

namespace pw_async_fuchsia {

constexpr pw::chrono::SystemClock::time_point ZxTimeToTimepoint(zx::time time) {
  timespec ts = time.to_timespec();
  auto duration = std::chrono::seconds{ts.tv_sec} + std::chrono::nanoseconds{ts.tv_nsec};
  return pw::chrono::SystemClock::time_point{duration};
}

constexpr zx::time TimepointToZxTime(pw::chrono::SystemClock::time_point tp) {
  auto seconds = std::chrono::time_point_cast<std::chrono::seconds>(tp);
  auto ns = std::chrono::time_point_cast<std::chrono::nanoseconds>(tp) -
            std::chrono::time_point_cast<std::chrono::nanoseconds>(seconds);

  return zx::time{timespec{seconds.time_since_epoch().count(), ns.count()}};
}

}  // namespace pw_async_fuchsia

#endif  // THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_UTIL_H_
