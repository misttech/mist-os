// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.ieee80211/cpp/fidl.h>
#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>

#include <gtest/gtest.h>
#include <src/connectivity/wlan/drivers/wlansoftmac/convert.h>
#include <wlan/drivers/log_instance.h>
#include <wlan/drivers/test/log_overrides.h>

#include "fuchsia/wlan/softmac/c/banjo.h"

namespace wlan::drivers {
namespace {
namespace wlan_softmac = fuchsia_wlan_softmac::wire;
namespace wlan_common = fuchsia_wlan_common::wire;

/* Metadata which is used as input and expected output for the under-test conversion functions*/

// Fake metadata -- general
static constexpr size_t kFakePacketSize = 50;

static constexpr uint8_t kRandomPopulaterUint8 = 118;
static constexpr uint16_t kRandomPopulaterUint16 = 53535;
static constexpr uint32_t kRandomPopulaterUint32 = 4062722468;

// Fake metadata -- FIDL
static constexpr wlan_common::WlanPhyType kFakeFidlPhyType = wlan_common::WlanPhyType::kErp;
static constexpr wlan_common::ChannelBandwidth kFakeFidlChannelBandwidth =
    wlan_common::ChannelBandwidth::kCbw160;

// Fake metadata -- banjo
static constexpr uint32_t kFakeBanjoPhyType = WLAN_PHY_TYPE_ERP;
static constexpr uint32_t kFakeBanjoChannelBandwidth = CHANNEL_BANDWIDTH_CBW160;

/* Test cases*/

class ConvertTest : public LogTest {};

// banjo to FIDL types tests.
TEST_F(ConvertTest, ToFidlTxPacket) {
  log::Instance::Init(0);
  // Populate wlan_tx_info_t
  uint8_t* data_in = (uint8_t*)calloc(kFakePacketSize, sizeof(uint8_t));
  for (size_t i = 0; i < kFakePacketSize; i++) {
    data_in[i] = kRandomPopulaterUint8;
  }

  wlan_tx_info_t info_in = {
      .tx_flags = kRandomPopulaterUint8,
      .valid_fields = kRandomPopulaterUint32,
      .tx_vector_idx = kRandomPopulaterUint16,
      .phy = kFakeBanjoPhyType,  // Valid PhyType in first try.
      .channel_bandwidth = kFakeBanjoChannelBandwidth,
      .mcs = kRandomPopulaterUint8,
  };

  // Conduct conversion
  wlan_softmac::WlanTxPacket out;
  EXPECT_EQ(ZX_OK, ConvertTxPacket(data_in, kFakePacketSize, info_in, &out));

  // Verify outputs
  EXPECT_EQ(kFakePacketSize, out.mac_frame.count());
  for (size_t i = 0; i < kFakePacketSize; i++) {
    EXPECT_EQ(kRandomPopulaterUint8, out.mac_frame.data()[i]);
  }

  EXPECT_EQ(kRandomPopulaterUint8, out.info.tx_flags);
  EXPECT_EQ(kRandomPopulaterUint32, out.info.valid_fields);
  EXPECT_EQ(kRandomPopulaterUint16, out.info.tx_vector_idx);
  EXPECT_EQ(kFakeFidlPhyType, out.info.phy);
  EXPECT_EQ(kFakeFidlChannelBandwidth, out.info.channel_bandwidth);
  EXPECT_EQ(kRandomPopulaterUint8, out.info.mcs);

  // Assign invalid values to the enum fields and verify the error returned.
  info_in.phy = kRandomPopulaterUint32;
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, ConvertTxPacket(data_in, kFakePacketSize, info_in, &out));

  info_in.phy = kFakeBanjoPhyType;
  info_in.channel_bandwidth = kRandomPopulaterUint32;
  EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, ConvertTxPacket(data_in, kFakePacketSize, info_in, &out));

  free(data_in);
}

}  // namespace
}  // namespace wlan::drivers
