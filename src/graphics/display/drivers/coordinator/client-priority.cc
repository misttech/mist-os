// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/client-priority.h"

#include <zircon/assert.h>

#include <cinttypes>

namespace display {

const char* DebugStringFromClientPriority(ClientPriority client_priority) {
  switch (client_priority) {
    case ClientPriority::kPrimary:
      return "primary";
    case ClientPriority::kVirtcon:
      return "virtcon";
  }
  ZX_DEBUG_ASSERT_MSG(false, "Invalid ClientPriority: %" PRId8,
                      static_cast<int8_t>(client_priority));
  return nullptr;
}

}  // namespace display
