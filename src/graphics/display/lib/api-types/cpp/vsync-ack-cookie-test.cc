// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/vsync-ack-cookie.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace display {

namespace {

constexpr VsyncAckCookie kOne(1);
constexpr VsyncAckCookie kTwo(2);

constexpr uint64_t kLargeCookieValue = uint64_t{1} << 63;
constexpr VsyncAckCookie kLargeCookie(kLargeCookieValue);

TEST(VsyncAckCookieTest, ToFidlVsyncAckCookie) {
  static constexpr fuchsia_hardware_display::wire::VsyncAckCookie kFidlOne = {.value = 1};
  EXPECT_EQ(kFidlOne.value, ToFidlVsyncAckCookie(kOne).value);

  static constexpr fuchsia_hardware_display::wire::VsyncAckCookie kFidlTwo = {.value = 2};
  EXPECT_EQ(kFidlTwo.value, ToFidlVsyncAckCookie(kTwo).value);

  static constexpr fuchsia_hardware_display::wire::VsyncAckCookie kFidlLargeCookie = {
      .value = kLargeCookieValue};
  EXPECT_EQ(kFidlLargeCookie.value, ToFidlVsyncAckCookie(kLargeCookie).value);

  static constexpr fuchsia_hardware_display::wire::VsyncAckCookie kFidlInvalidCookie = {
      .value = fuchsia_hardware_display_types::wire::kInvalidDispId};
  EXPECT_EQ(kFidlInvalidCookie.value, ToFidlVsyncAckCookie(kInvalidVsyncAckCookie).value);
}

TEST(VsyncAckCookieTest, ToVsyncAckCookieWithFidlVsyncAckCookie) {
  static constexpr fuchsia_hardware_display::wire::VsyncAckCookie kFidlOne = {.value = 1};
  EXPECT_EQ(kOne, ToVsyncAckCookie(kFidlOne));

  static constexpr fuchsia_hardware_display::wire::VsyncAckCookie kFidlTwo = {.value = 2};
  EXPECT_EQ(kTwo, ToVsyncAckCookie(kFidlTwo));

  static constexpr fuchsia_hardware_display::wire::VsyncAckCookie kFidlLargeCookie = {
      .value = kLargeCookieValue};
  EXPECT_EQ(kLargeCookie, ToVsyncAckCookie(kFidlLargeCookie));

  static constexpr fuchsia_hardware_display::wire::VsyncAckCookie kFidlInvalidCookie = {
      .value = fuchsia_hardware_display_types::wire::kInvalidDispId};
  EXPECT_EQ(kInvalidVsyncAckCookie, ToVsyncAckCookie(kFidlInvalidCookie));
}

TEST(VsyncAckCookieTest, FidlVsyncAckCookieConversionRoundtrip) {
  EXPECT_EQ(kOne, ToVsyncAckCookie(ToFidlVsyncAckCookie(kOne)));
  EXPECT_EQ(kTwo, ToVsyncAckCookie(ToFidlVsyncAckCookie(kTwo)));
  EXPECT_EQ(kLargeCookie, ToVsyncAckCookie(ToFidlVsyncAckCookie(kLargeCookie)));
  EXPECT_EQ(kInvalidVsyncAckCookie, ToVsyncAckCookie(ToFidlVsyncAckCookie(kInvalidVsyncAckCookie)));
}

}  // namespace

}  // namespace display
