// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_LIB_MLME_CPP_TESTS_TEST_UTILS_H_
#define SRC_CONNECTIVITY_WLAN_LIB_MLME_CPP_TESTS_TEST_UTILS_H_

#include <algorithm>
#include <array>
#include <memory>
#include <vector>

#include <gtest/gtest.h>
#include <wlan/common/buffer_writer.h>
#include <wlan/mlme/mac_frame.h>
#include <wlan/mlme/packet.h>

namespace wlan {
namespace test_utils {

template <typename T>
struct RangeWrapper {
  explicit RangeWrapper(const T& range) : range(range) {}

  const T& range;

  auto begin() const { return std::begin(range); }
  auto end() const { return std::end(range); }

  template <typename U>
  bool operator==(const RangeWrapper<U>& other) const {
    return std::equal(begin(), end(), other.begin(), other.end());
  }
};

}  // namespace test_utils
}  // namespace wlan

#define EXPECT_RANGES_EQ(a, b) \
  EXPECT_EQ(test_utils::RangeWrapper((a)), test_utils::RangeWrapper((b)))

#define ASSERT_RANGES_EQ(a, b) \
  ASSERT_EQ(test_utils::RangeWrapper((a)), test_utils::RangeWrapper((b)))

#define LIST_MAC_ADDR_BYTES(a) \
  (a).byte[0], (a).byte[1], (a).byte[2], (a).byte[3], (a).byte[4], (a).byte[5]

// clang-format off
#define LIST_UINT32_BYTES(x) static_cast<uint8_t>((x) & 0xff), \
                             static_cast<uint8_t>(((x) >> 8) & 0xff), \
                             static_cast<uint8_t>(((x) >> 16) & 0xff), \
                             static_cast<uint8_t>(((x) >> 24) & 0xff)
// clang-format on

#endif  // SRC_CONNECTIVITY_WLAN_LIB_MLME_CPP_TESTS_TEST_UTILS_H_
