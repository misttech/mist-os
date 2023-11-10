// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/wlan/ieee80211/c/banjo.h>

#include <algorithm>
#include <cstring>
#include <memory>
#include <utility>

#include <gtest/gtest.h>
#include <wlan/common/buffer_writer.h>
#include <wlan/mlme/mac_frame.h>
#include <wlan/mlme/wlan.h>

namespace wlan {
namespace {

template <typename T>
T AvoidReferenceBindingToMisalignedAddress(T t) {
  return t;
}

class Elements : public ::testing::Test {
 protected:
  Elements() { buf_offset_ = buf_; }

  template <typename T>
  void add_to_buf(const T& value) {
    memcpy(buf_offset_, &value, sizeof(value));
    buf_offset_ += sizeof(value);
  }
  uint8_t* buf_offset_;
  uint8_t buf_[1024] = {};
  size_t actual_ = 0;
};

TEST(HtCapabilities, DdkConversion) {
  ht_capabilities_t ddk{
      .bytes =
          {
              0x6e, 0x01,  // HtCapabilityInfo
              0x17,        // AmpduParams
              0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xff,
              0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // SupportedMcsSet
              0x34, 0x12,                                      // HtExtCapabilities
              0x78, 0x56, 0x34, 0x12,                          // TxBeamformingCapabilities
              0xff,                                            // AselCapability
          },
  };

  auto ieee = HtCapabilities::FromDdk(ddk);
  EXPECT_EQ(0x016eU, ieee.ht_cap_info.as_uint16());
  EXPECT_EQ(0x17U, ieee.ampdu_params.val());
  std::array<uint8_t, 16> expected_mcs_set = {0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xff,
                                              0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00};
  EXPECT_EQ(expected_mcs_set, ieee.mcs_set.val());
  EXPECT_EQ(0x1234U, ieee.ht_ext_cap.as_uint16());
  EXPECT_EQ(0x12345678U, ieee.txbf_cap.as_uint32());
  EXPECT_EQ(0xffU, ieee.asel_cap.val());

  auto ddk2 = ieee.ToDdk();
  for (size_t i = 0; i < sizeof(ddk.bytes); i++) {
    EXPECT_EQ(ddk.bytes[i], ddk2.bytes[i]);
  }
}

TEST(VhtCapabilities, DdkConversion) {
  vht_capabilities_t ddk{
      .bytes =
          {
              0xdd, 0xcc, 0xbb, 0xaa,                         // VhtCapabilityInfo
              0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00  // SupportedVhtMcsAndNssSet
          },
  };

  auto ieee = VhtCapabilities::FromDdk(ddk);
  EXPECT_EQ(0xaabbccddU, ieee.vht_cap_info.as_uint32());
  EXPECT_EQ(0x0011223344556677U, ieee.vht_mcs_nss.as_uint64());

  auto ddk2 = ieee.ToDdk();
  EXPECT_EQ(0, memcmp(ddk.bytes, ddk2.bytes, sizeof(ddk.bytes)));
}

}  // namespace
}  // namespace wlan
