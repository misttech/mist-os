// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/driver.h"

namespace display {
Driver::Driver(Controller* controller, zx_device_t* parent)
    : controller_(controller), parent_(parent) {
  ZX_DEBUG_ASSERT(controller);
  ZX_DEBUG_ASSERT(parent);
}

Driver::~Driver() { zxlogf(TRACE, "Driver::~Driver"); }
}  // namespace display
