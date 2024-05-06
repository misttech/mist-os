// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/time.h>
#include <zircon/time.h>

#include <platform/timer.h>

zx_time_t zx_clock_get_monotonic(void) { return current_time(); }

zx_time_t zx_ticks_per_second(void) { return ticks_per_second(); }
