// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_TESTS_INTEGRATION_INPUT_TESTS_STARNIX_TOUCH_RELAY_API_H_
#define SRC_UI_TESTS_INTEGRATION_INPUT_TESTS_STARNIX_TOUCH_RELAY_API_H_

#include <cstddef>
#include <cstdint>

namespace relay_api {

constexpr char kReadyMessage[] = "READY";
constexpr char kFailedMessage[] = "FAILED";
constexpr char kEventDelimiter[] = "EVENT";
constexpr char kEventFormat[] = "EVENT tv_sec=%ld tv_usec=%ld type=%hu code=%hu value=%d";

// The formatted event string will be sent across systems, so verify that the
// size of a `long` is the same on both sides. Similarly for `unsigned short`.
static_assert(sizeof(long) == sizeof(int64_t));
static_assert(sizeof(unsigned short) == sizeof(uint16_t));

namespace internal {
constexpr size_t kMaxDigitsPerLong = 20;          // -9,223,372,036,854,775,808
constexpr size_t kMaxDigitsPerUnsignedShort = 5;  // 65,535
}  // namespace internal

constexpr size_t kMaxPacketLen = sizeof(kEventFormat) + 3 * internal::kMaxDigitsPerLong +
                                 2 * internal::kMaxDigitsPerUnsignedShort;

// Touch down is expressed in six `uapi::input_event`s: BTN_TOUCH, ABS_MT_SLOT,
// ABS_MT_TRACKING_ID, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, and EV_SYN.
// Touch up is expressed in four: BTN_TOUCH, ABS_MT_SLOT, ABS_MT_TRACKING_ID,
// and EV_SYN.
constexpr size_t kDownUpNumPackets = 10;

}  // namespace relay_api

#endif  // SRC_UI_TESTS_INTEGRATION_INPUT_TESTS_STARNIX_TOUCH_RELAY_API_H_
