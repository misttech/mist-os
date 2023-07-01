// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_PRIORITY_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_PRIORITY_H_

#include <cstdint>

namespace display {

// The presentation priority level of a client connected to the Coordinator.
//
// Currently, at most one client can be connected at each priority level.
enum class ClientPriority : int8_t {
  kVirtcon = 0,
  kPrimary = 1,
};

const char* DebugStringFromClientPriority(ClientPriority client_priority);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_CLIENT_PRIORITY_H_
