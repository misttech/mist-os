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
namespace wlan_ieee80211 = fuchsia_wlan_ieee80211::wire;

/* Metadata which is used as input and expected output for the under-test conversion functions*/

// Fake metadata -- general
static constexpr uint8_t kFakeMacAddr[wlan_ieee80211::kMacAddrLen] = {6, 5, 4, 3, 2, 2};
static constexpr uint8_t kFakeOui[wlan_ieee80211::kOuiLen] = {9, 7, 1};
static constexpr uint8_t kFakeChannel = 15;
static constexpr uint8_t kFakeKey[wlan_ieee80211::kMaxKeyLen] = {
    6, 9, 3, 9, 9, 3, 7, 5, 1, 0, 5, 8, 2, 0, 9, 7, 4, 9, 4, 4, 5, 9, 2, 3, 0, 7, 8, 1, 6, 4, 0, 6};
static constexpr size_t kFakePacketSize = 50;

static constexpr bool kPopulaterBool = true;
static constexpr uint8_t kRandomPopulaterUint8 = 118;
static constexpr uint16_t kRandomPopulaterUint16 = 53535;
static constexpr uint32_t kRandomPopulaterUint32 = 4062722468;
static constexpr uint64_t kRandomPopulaterUint64 = 1518741085930693;
static constexpr int8_t kRandomPopulaterInt8 = -95;
static constexpr int16_t kRandomPopulaterInt16 = -24679;

// Fake metadata -- FIDL
static constexpr wlan_common::WlanMacRole kFakeFidlMacRole = wlan_common::WlanMacRole::kAp;
static constexpr wlan_common::WlanPhyType kFakeFidlPhyType = wlan_common::WlanPhyType::kErp;
static constexpr wlan_common::ChannelBandwidth kFakeFidlChannelBandwidth =
    wlan_common::ChannelBandwidth::kCbw160;
static constexpr wlan_softmac::WlanProtection kFakeFidlProtection =
    wlan_softmac::WlanProtection::kRxTx;
static constexpr wlan_common::WlanKeyType kFakeFidlKeyType = wlan_common::WlanKeyType::kGroup;
static constexpr wlan_common::BssType kFakeFidlBssType = wlan_common::BssType::kMesh;
static constexpr wlan_common::WlanTxResultCode kFakeFidlTxResultCode =
    wlan_common::WlanTxResultCode::kSuccess;
static constexpr wlan_softmac::WlanRxInfoFlags kFakeRxFlags =
    wlan_softmac::WlanRxInfoFlags::TruncatingUnknown(kRandomPopulaterUint32);
static constexpr wlan_softmac::WlanRxInfoValid kFakeRxValid =
    wlan_softmac::WlanRxInfoValid::TruncatingUnknown(kRandomPopulaterUint32);

// Fake metadata -- banjo
static constexpr uint32_t kFakeBanjoMacRole = WLAN_MAC_ROLE_AP;
static constexpr uint32_t kFakeBanjoPhyType = WLAN_PHY_TYPE_ERP;
static constexpr uint32_t kFakeBanjoChannelBandwidth = CHANNEL_BANDWIDTH_CBW160;
static constexpr uint8_t kFakeBanjoProtection = WLAN_PROTECTION_RX_TX;
static constexpr uint8_t kFakeBanjoKeyType = WLAN_KEY_TYPE_GROUP;
static constexpr uint32_t kFakeBanjoBssType = BSS_TYPE_MESH;
static constexpr uint8_t kFakeBanjoTxResultCode = WLAN_TX_RESULT_CODE_SUCCESS;

/* Test cases*/

class ConvertTest : public LogTest {};

// FIDL to banjo types tests.

TEST_F(ConvertTest, ToBanjoRxPacket) {
  log::Instance::Init(0);
  // Populate wlan_softmac::WlanRxPacket
  uint8_t* rx_packet = (uint8_t*)calloc(kFakePacketSize, sizeof(uint8_t));
  for (size_t i = 0; i < kFakePacketSize; i++) {
    rx_packet[i] = kRandomPopulaterUint8;
  }

  wlan_softmac::WlanRxPacket in = {
      .mac_frame = fidl::VectorView<uint8_t>::FromExternal(rx_packet, kFakePacketSize),
      .info =
          {
              .rx_flags = kFakeRxFlags,
              .valid_fields = kFakeRxValid,
              .phy = kFakeFidlPhyType,
              .data_rate = kRandomPopulaterUint32,
              .channel =
                  {
                      .primary = kFakeChannel,
                      .cbw = kFakeFidlChannelBandwidth,
                      .secondary80 = kFakeChannel,
                  },
              .mcs = kRandomPopulaterUint8,
              .rssi_dbm = kRandomPopulaterInt8,
              .snr_dbh = kRandomPopulaterInt16,
          },
  };

  // Conduct conversion
  wlan_rx_packet_t out;
  uint8_t out_packet_buffer[kFakePacketSize];
  EXPECT_EQ(ZX_OK, ConvertRxPacket(in, &out, out_packet_buffer));

  // Verify outputs
  EXPECT_EQ(kFakePacketSize, out.mac_frame_size);
  for (size_t i = 0; i < kFakePacketSize; i++) {
    EXPECT_EQ(kRandomPopulaterUint8, out.mac_frame_buffer[i]);
  }

  EXPECT_EQ(static_cast<uint32_t>(kFakeRxFlags), out.info.rx_flags);
  EXPECT_EQ(static_cast<uint32_t>(kFakeRxValid), out.info.valid_fields);
  EXPECT_EQ(kFakeBanjoPhyType, out.info.phy);
  EXPECT_EQ(kRandomPopulaterUint32, out.info.data_rate);
  EXPECT_EQ(kFakeChannel, out.info.channel.primary);
  EXPECT_EQ(kFakeBanjoChannelBandwidth, out.info.channel.cbw);
  EXPECT_EQ(kFakeChannel, out.info.channel.secondary80);
  EXPECT_EQ(kRandomPopulaterUint8, out.info.mcs);
  EXPECT_EQ(kRandomPopulaterInt8, out.info.rssi_dbm);
  EXPECT_EQ(kRandomPopulaterInt16, out.info.snr_dbh);

  free(rx_packet);
}  // namespace

TEST_F(ConvertTest, ToBanjoTxStatus) {
  log::Instance::Init(0);
  // Populate wlan_common::WlanTxResult
  wlan_common::WlanTxResult in = {
      .result_code = kFakeFidlTxResultCode,
  };
  for (size_t i = 0; i < wlan_common::kWlanTxResultMaxEntry; i++) {
    in.tx_result_entry[i].tx_vector_idx = kRandomPopulaterUint16;
    in.tx_result_entry[i].attempts = kRandomPopulaterUint8;
  }
  for (size_t i = 0; i < wlan_ieee80211::kMacAddrLen; i++) {
    in.peer_addr[i] = kFakeMacAddr[i];
  }

  // Conduct conversion
  wlan_tx_result_t out;
  EXPECT_EQ(ZX_OK, ConvertTxStatus(in, &out));

  // Verify outputs
  for (size_t i = 0; i < wlan_common::kWlanTxResultMaxEntry; i++) {
    EXPECT_EQ(kRandomPopulaterUint16, out.tx_result_entry[i].tx_vector_idx);
    EXPECT_EQ(kRandomPopulaterUint8, out.tx_result_entry[i].attempts);
  }

  for (size_t i = 0; i < wlan_ieee80211::kMacAddrLen; i++) {
    EXPECT_EQ(kFakeMacAddr[i], out.peer_addr[i]);
  }
  EXPECT_EQ(kFakeBanjoTxResultCode, out.result_code);
}

// banjo to FIDL types tests.
TEST_F(ConvertTest, ToFidlMacRole) {
  log::Instance::Init(0);
  wlan_common::WlanMacRole out;
  EXPECT_EQ(ZX_OK, ConvertMacRole(kFakeBanjoMacRole, &out));

  EXPECT_EQ(kFakeFidlMacRole, out);

  // Input the invalid value, and the conversion will fail.
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, ConvertMacRole(kRandomPopulaterUint32, &out));
}

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

TEST_F(ConvertTest, ToFidlChannel) {
  log::Instance::Init(0);
  wlan_channel_t in = {
      .primary = kFakeChannel,
      .cbw = kFakeBanjoChannelBandwidth,
      .secondary80 = kFakeChannel,
  };

  wlan_common::WlanChannel out;
  EXPECT_EQ(ZX_OK, ConvertChannel(in, &out));

  EXPECT_EQ(kFakeChannel, out.primary);
  EXPECT_EQ(kFakeFidlChannelBandwidth, out.cbw);
  EXPECT_EQ(kFakeChannel, out.secondary80);

  // Assign an invalid value to cbw, and the conversion will fail.
  in.cbw = kRandomPopulaterUint32;
  EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, ConvertChannel(in, &out));
}

TEST_F(ConvertTest, ToFidlJoinBssRequest) {
  log::Instance::Init(0);
  // Populate join_bss_request_t
  join_bss_request_t in = {
      .bss_type = kFakeBanjoBssType,
      .remote = kPopulaterBool,
      .beacon_period = kRandomPopulaterUint16,
  };
  for (size_t i = 0; i < wlan_ieee80211::kMacAddrLen; i++) {
    in.bssid[i] = kFakeMacAddr[i];
  }

  // Conduct conversion
  wlan_common::JoinBssRequest out;
  fidl::Arena fidl_arena;
  EXPECT_EQ(ZX_OK, ConvertJoinBssRequest(in, &out, fidl_arena));

  // Verify outputs
  for (size_t i = 0; i < wlan_ieee80211::kMacAddrLen; i++) {
    EXPECT_EQ(kFakeMacAddr[i], out.bssid().data()[i]);
  }
  EXPECT_EQ(kFakeFidlBssType, out.bss_type());
  EXPECT_EQ(kPopulaterBool, out.remote());
  EXPECT_EQ(kRandomPopulaterUint16, out.beacon_period());

  // Assign an invalid value to cbw, and the conversion will fail.
  in.bss_type = kRandomPopulaterUint32;
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, ConvertJoinBssRequest(in, &out, fidl_arena));
}

TEST_F(ConvertTest, ToFidlKeyConfig) {
  log::Instance::Init(0);
  // Create tmp non-const key from const key.
  uint8_t TmpKey[wlan_ieee80211::kMaxKeyLen];
  memcpy(TmpKey, kFakeKey, wlan_ieee80211::kMaxKeyLen);

  // Populate wlan_key_configuration_t
  wlan_key_configuration_t in = {
      .protection = kFakeBanjoProtection,
      .cipher_type = kRandomPopulaterUint8,
      .key_type = kFakeBanjoKeyType,
      .key_idx = kRandomPopulaterUint8,
      .key_list = TmpKey,
      .key_count = wlan_ieee80211::kMaxKeyLen,
      .rsc = kRandomPopulaterUint64,
  };

  for (size_t i = 0; i < wlan_ieee80211::kOuiLen; i++) {
    in.cipher_oui[i] = kFakeOui[i];
  }

  for (size_t i = 0; i < wlan_ieee80211::kMacAddrLen; i++) {
    in.peer_addr[i] = kFakeMacAddr[i];
  }

  // Conduct conversion
  fidl::Arena arena;
  wlan_softmac::WlanKeyConfiguration out;
  EXPECT_EQ(ZX_OK, ConvertKeyConfig(in, &out, arena));

  // Verify outputs
  EXPECT_EQ(kFakeFidlProtection, out.protection());
  EXPECT_EQ(kRandomPopulaterUint8, out.cipher_type());
  EXPECT_EQ(kFakeFidlKeyType, out.key_type());
  EXPECT_EQ(kRandomPopulaterUint8, out.key_idx());
  EXPECT_EQ(kRandomPopulaterUint64, out.rsc());

  for (size_t i = 0; i < wlan_ieee80211::kOuiLen; i++) {
    EXPECT_EQ(kFakeOui[i], out.cipher_oui().data()[i]);
  }

  for (size_t i = 0; i < wlan_ieee80211::kMacAddrLen; i++) {
    EXPECT_EQ(kFakeMacAddr[i], out.peer_addr().data()[i]);
  }

  EXPECT_EQ(wlan_ieee80211::kMaxKeyLen, out.key().count());
  for (size_t i = 0; i < wlan_ieee80211::kMaxKeyLen; i++) {
    EXPECT_EQ(kFakeKey[i], out.key().data()[i]);
  }
}

}  // namespace
}  // namespace wlan::drivers
