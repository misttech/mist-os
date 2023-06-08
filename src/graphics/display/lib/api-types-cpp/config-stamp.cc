// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>

namespace display {

static_assert(kInvalidConfigStamp.value() == INVALID_CONFIG_STAMP_VALUE);

}  // namespace display
