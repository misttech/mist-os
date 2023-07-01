// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_ID_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_ID_H_

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// Typesafe unique identifier for an active Coordinator client connection.
DEFINE_STRONG_INT(ClientId, uint64_t);

constexpr ClientId kInvalidClientId(0);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_ID_H_
