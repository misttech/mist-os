// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/util.h"

namespace display {

#define DEFINE_BINARY_OPERATOR(TYPE, OP) \
  bool operator OP(const TYPE& a, const TYPE& b) { return (a.value)OP(b.value); }

DEFINE_BINARY_OPERATOR(fuchsia_hardware_display::wire::ConfigStamp, ==)
DEFINE_BINARY_OPERATOR(fuchsia_hardware_display::wire::ConfigStamp, !=)
DEFINE_BINARY_OPERATOR(fuchsia_hardware_display::wire::ConfigStamp, >)
DEFINE_BINARY_OPERATOR(fuchsia_hardware_display::wire::ConfigStamp, >=)
DEFINE_BINARY_OPERATOR(fuchsia_hardware_display::wire::ConfigStamp, <)
DEFINE_BINARY_OPERATOR(fuchsia_hardware_display::wire::ConfigStamp, <=)

#undef DEFINE_BINARY_OPERATOR

}  // namespace display
