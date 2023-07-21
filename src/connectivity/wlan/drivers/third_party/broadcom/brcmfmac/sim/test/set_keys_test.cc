// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-device/device.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-env.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"
#include "zircon/errors.h"

namespace wlan::brcmfmac {

class SetKeysTest : public SimTest {
 public:
  SetKeysTest() = default;
  void SetUp() override {
    ASSERT_EQ(ZX_OK, SimTest::Init());
    ASSERT_EQ(ZX_OK, StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc_));
  }
  void TearDown() override { EXPECT_EQ(SimTest::DeleteInterface(&client_ifc_), ZX_OK); }

  SimInterface client_ifc_;
};

wlan_fullmac::WlanFullmacSetKeysReq FakeSetKeysRequest(
    const uint8_t keys[][wlan_ieee80211::kMaxKeyLen], size_t n) {
  wlan_fullmac::WlanFullmacSetKeysReq set_keys_req{.num_keys = n};

  for (size_t i = 0; i < n; i++) {
    set_keys_req.keylist[i] = {
        .key = fidl::VectorView<uint8_t>::FromExternal(
            const_cast<uint8_t*>(keys[i]), strlen(reinterpret_cast<const char*>(keys[i]))),
        .key_id = static_cast<uint16_t>(i),
        .key_type = fuchsia_wlan_common::wire::WlanKeyType::kPairwise,
        .cipher_suite_type = wlan_ieee80211::CipherSuiteType::kCcmp128,
    };
  }
  return set_keys_req;
}

TEST_F(SetKeysTest, MultipleKeys) {
  const uint8_t keys[wlan_fullmac::kWlanMaxKeylistSize][wlan_ieee80211::kMaxKeyLen] = {
      "One", "Two", "Three", "Four"};
  wlan_fullmac::WlanFullmacSetKeysReq set_keys_req =
      FakeSetKeysRequest(keys, wlan_fullmac::kWlanMaxKeylistSize);
  auto result = client_ifc_.client_.buffer(client_ifc_.test_arena_)->SetKeysReq(set_keys_req);
  EXPECT_TRUE(result.ok());

  auto& set_keys_resp = result->resp;

  std::vector<brcmf_wsec_key_le> firmware_keys =
      device_->GetSim()->sim_fw->GetKeyList(client_ifc_.iface_id_);
  ASSERT_EQ(firmware_keys.size(), wlan_fullmac::kWlanMaxKeylistSize);
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[0].data), "One");
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[1].data), "Two");
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[2].data), "Three");
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[3].data), "Four");
  ASSERT_EQ(set_keys_resp.num_keys, wlan_fullmac::kWlanMaxKeylistSize);
  for (auto status : set_keys_resp.statuslist.data_) {
    ASSERT_EQ(status, ZX_OK);
  }
}

// Ensure that group key is set correctly by the driver in firmware.
TEST_F(SetKeysTest, SetGroupKey) {
  const uint8_t group_key[wlan_ieee80211::kMaxKeyLen] = "Group Key";
  const uint8_t ucast_key[wlan_ieee80211::kMaxKeyLen] = "My Key";
  const size_t kKeyNum = 2;

  fidl::Array<wlan_fullmac::SetKeyDescriptor, wlan_fullmac::kWlanMaxKeylistSize> key_list = {
      .data_ =
          {
              {
                  .key = fidl::VectorView<uint8_t>::FromExternal(
                      const_cast<uint8_t*>(group_key),
                      strlen(reinterpret_cast<const char*>(group_key))),
                  .key_id = 0,
                  .key_type = fuchsia_wlan_common::wire::WlanKeyType::kGroup,
                  .address = {0xff, 0xff, 0xff, 0xff, 0xff, 0xff},
                  .cipher_suite_type = wlan_ieee80211::CipherSuiteType::kCcmp128,
              },
              {
                  .key = fidl::VectorView<uint8_t>::FromExternal(
                      const_cast<uint8_t*>(ucast_key),
                      strlen(reinterpret_cast<const char*>(ucast_key))),
                  .key_id = 1,
                  .key_type = fuchsia_wlan_common::wire::WlanKeyType::kPairwise,
                  .address = {0xde, 0xad, 0xbe, 0xef, 0xab, 0xcd},
                  .cipher_suite_type = wlan_ieee80211::CipherSuiteType::kCcmp128,
              },
          },
  };

  wlan_fullmac::WlanFullmacSetKeysReq key_req = {
      .num_keys = kKeyNum,
      .keylist = {key_list},
  };

  // Set all the remaining enums in the request to valid values to pass FIDL enum value check during
  // the FIDL call.
  // TODO(b/288173116): Remove this after changing keylist to vector.
  for (size_t i = 2; i < wlan_fullmac::kWlanMaxKeylistSize; i++) {
    key_req.keylist.data()[i].key_type = fuchsia_wlan_common::wire::WlanKeyType::kPairwise;
    key_req.keylist.data()[i].cipher_suite_type = wlan_ieee80211::CipherSuiteType::kCcmp128;
  }
  auto result = client_ifc_.client_.buffer(client_ifc_.test_arena_)->SetKeysReq(key_req);
  EXPECT_TRUE(result.ok());

  auto& set_keys_resp = result->resp;
  ASSERT_EQ(set_keys_resp.num_keys, 2ul);
  ASSERT_EQ(set_keys_resp.statuslist.data()[0], ZX_OK);
  ASSERT_EQ(set_keys_resp.statuslist.data()[1], ZX_OK);

  std::vector<brcmf_wsec_key_le> firmware_keys =
      device_->GetSim()->sim_fw->GetKeyList(client_ifc_.iface_id_);
  ASSERT_EQ(firmware_keys.size(), 2U);
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[0].data),
               reinterpret_cast<const char*>(group_key));
  // Group key should have been set as the PRIMARY KEY
  ASSERT_EQ(firmware_keys[0].flags, (const unsigned int)BRCMF_PRIMARY_KEY);
  EXPECT_STREQ(reinterpret_cast<const char*>(firmware_keys[1].data),
               reinterpret_cast<const char*>(ucast_key));
  ASSERT_NE(firmware_keys[1].flags, (const unsigned int)BRCMF_PRIMARY_KEY);
}
}  // namespace wlan::brcmfmac
