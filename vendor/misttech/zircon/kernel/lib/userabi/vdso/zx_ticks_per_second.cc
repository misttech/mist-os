// Copyright 2016, 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "data-time-values.h"
#include "private.h"

__EXPORT zx_ticks_t _zx_ticks_per_second(void) { return DATA_TIME_VALUES.ticks_per_second; }

VDSO_INTERFACE_FUNCTION(zx_ticks_per_second);
