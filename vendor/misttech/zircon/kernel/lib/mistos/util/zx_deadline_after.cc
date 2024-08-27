// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/time.h>
#include <zircon/time.h>

zx_time_t zx_deadline_after(zx_duration_t nanoseconds) {
  zx_time_t now = zx_clock_get_monotonic();
  return zx_time_add_duration(now, nanoseconds);
}
