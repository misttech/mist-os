// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/vendor/marvell/host_channel_manager.h"

#include <zxtest/zxtest.h>

namespace bt_hci_marvell {

class HostChannelManagerTest : public zxtest::Test {
 protected:
  HostChannelManager ch_mgr_;
};

TEST_F(HostChannelManagerTest, BasicOperations) {
  zx::channel acldata_vendor_raw_ch;
  const HostChannel* acldata_vendor_ch;
  constexpr uint64_t kAclDataToVendorKey = 0x1234;
  acldata_vendor_ch = ch_mgr_.AddChannel(
      std::move(acldata_vendor_raw_ch), ControllerChannelId::kChannelAclData,
      ControllerChannelId::kChannelVendor, kAclDataToVendorKey, "acldata_vendor");
  EXPECT_NE(nullptr, acldata_vendor_ch);

  // Test HostChannelFromInterruptKey() - result found
  EXPECT_EQ(acldata_vendor_ch->interrupt_key(), kAclDataToVendorKey);
  EXPECT_EQ(acldata_vendor_ch, ch_mgr_.HostChannelFromInterruptKey(kAclDataToVendorKey));

  // Test HostChannelFromInterruptKey() - result not found
  EXPECT_EQ(nullptr, ch_mgr_.HostChannelFromInterruptKey(0x1235));

  // Verify that we can't add a channel with a duplicate write_id
  zx::channel command_vendor_ch;
  constexpr uint64_t kCommandToVendorKey = 0x5678;
  EXPECT_EQ(nullptr,
            ch_mgr_.AddChannel(std::move(command_vendor_ch), ControllerChannelId::kChannelCommand,
                               ControllerChannelId::kChannelVendor, kCommandToVendorKey,
                               "command_vendor"));

  // Verify that we can't add a channel with a duplicate interrupt_key
  zx::channel scodata_scodata_ch;
  constexpr uint64_t kScoDataToScoDataKey = kAclDataToVendorKey;
  EXPECT_EQ(nullptr, ch_mgr_.AddChannel(
                         std::move(scodata_scodata_ch), ControllerChannelId::kChannelScoData,
                         ControllerChannelId::kChannelScoData, kScoDataToScoDataKey, "sco_sco"));

  // And then if we search, we only find the first channel
  const HostChannel* lookup_result =
      ch_mgr_.HostChannelFromWriteId(ControllerChannelId::kChannelVendor);
  EXPECT_EQ(lookup_result, acldata_vendor_ch);

  // Verify we can remove a channel
  ch_mgr_.RemoveChannel(ControllerChannelId::kChannelVendor);
  lookup_result = ch_mgr_.HostChannelFromWriteId(ControllerChannelId::kChannelVendor);
  EXPECT_EQ(nullptr, lookup_result);

  // Verify that we can re-add a channel with the same write id once it's been deleted
  EXPECT_NE(nullptr,
            ch_mgr_.AddChannel(std::move(command_vendor_ch), ControllerChannelId::kChannelCommand,
                               ControllerChannelId::kChannelVendor, kCommandToVendorKey,
                               "command_vendor"));
}

TEST_F(HostChannelManagerTest, ForEveryChannel) {
  zx::channel command_acldata_raw_ch;
  constexpr uint64_t kCommandToAclDataKey = 0xabcd;
  EXPECT_NE(nullptr, ch_mgr_.AddChannel(std::move(command_acldata_raw_ch),
                                        ControllerChannelId::kChannelCommand,
                                        ControllerChannelId::kChannelAclData, kCommandToAclDataKey,
                                        "command_acldata"));

  zx::channel acldata_scodata_raw_ch;
  constexpr uint64_t kAclDataToScoDataKey = 0xfedcba9876543210;
  EXPECT_NE(nullptr, ch_mgr_.AddChannel(std::move(acldata_scodata_raw_ch),
                                        ControllerChannelId::kChannelAclData,
                                        ControllerChannelId::kChannelScoData, kAclDataToScoDataKey,
                                        "acldata_scodata"));

  zx::channel acldata_event_raw_ch;
  constexpr uint64_t kAclDataToEventKey = 0xffff;
  EXPECT_NE(nullptr, ch_mgr_.AddChannel(
                         std::move(acldata_event_raw_ch), ControllerChannelId::kChannelAclData,
                         ControllerChannelId::kChannelEvent, kAclDataToEventKey, "acldata_event"));

  // Verify that we have 3 channels
  int channel_count = 0;
  ch_mgr_.ForEveryChannel([&channel_count](const HostChannel* host_channel) { channel_count++; });
  EXPECT_EQ(channel_count, 3);

  // Verify that we have two channels that read AclData
  channel_count = 0;
  ch_mgr_.ForEveryChannel([&channel_count](const HostChannel* host_channel) {
    if (host_channel->read_id() == ControllerChannelId::kChannelAclData) {
      channel_count++;
    }
  });
  EXPECT_EQ(channel_count, 2);

  // Verify that we have no channels that write to a Vendor channel
  channel_count = 0;
  ch_mgr_.ForEveryChannel([&channel_count](const HostChannel* host_channel) {
    if (host_channel->write_id() == ControllerChannelId::kChannelVendor) {
      channel_count++;
    }
  });
  EXPECT_EQ(channel_count, 0);
}

}  // namespace bt_hci_marvell
