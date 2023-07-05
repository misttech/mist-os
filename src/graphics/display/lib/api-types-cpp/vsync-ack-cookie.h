// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_VSYNC_ACK_COOKIE_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_VSYNC_ACK_COOKIE_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `fuchsia.hardware.display/VsyncAckCookie`.
DEFINE_STRONG_INT(VsyncAckCookie, uint64_t);

constexpr inline VsyncAckCookie ToVsyncAckCookie(uint64_t fidl_vsync_ack_cookie_value) {
  return VsyncAckCookie(fidl_vsync_ack_cookie_value);
}
constexpr inline uint64_t ToFidlVsyncAckCookieValue(VsyncAckCookie vsync_ack_cookie) {
  return vsync_ack_cookie.value();
}

constexpr VsyncAckCookie kInvalidVsyncAckCookie(fuchsia_hardware_display::wire::kInvalidDispId);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_VSYNC_ACK_COOKIE_H_
