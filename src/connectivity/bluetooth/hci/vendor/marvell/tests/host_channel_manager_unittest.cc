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
  acldata_vendor_ch =
      ch_mgr_.AddChannel(std::move(acldata_vendor_raw_ch), ControllerChannelId::kChannelAclData,
                         ControllerChannelId::kChannelVendor, "acldata_vendor");
  EXPECT_NE(nullptr, acldata_vendor_ch);

  // Verify that we can't add a channel with a duplicate write_id
  zx::channel command_vendor_ch;
  EXPECT_EQ(nullptr,
            ch_mgr_.AddChannel(std::move(command_vendor_ch), ControllerChannelId::kChannelCommand,
                               ControllerChannelId::kChannelVendor, "command_vendor"));

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
                               ControllerChannelId::kChannelVendor, "command_vendor"));
}

TEST_F(HostChannelManagerTest, ForEveryChannel) {
  zx::channel command_acldata_raw_ch;
  EXPECT_NE(nullptr, ch_mgr_.AddChannel(std::move(command_acldata_raw_ch),
                                        ControllerChannelId::kChannelCommand,
                                        ControllerChannelId::kChannelAclData, "command_acldata"));

  zx::channel acldata_scodata_raw_ch;
  EXPECT_NE(nullptr, ch_mgr_.AddChannel(std::move(acldata_scodata_raw_ch),
                                        ControllerChannelId::kChannelAclData,
                                        ControllerChannelId::kChannelScoData, "acldata_scodata"));

  zx::channel acldata_event_raw_ch;
  EXPECT_NE(nullptr, ch_mgr_.AddChannel(std::move(acldata_event_raw_ch),
                                        ControllerChannelId::kChannelAclData,
                                        ControllerChannelId::kChannelEvent, "acldata_event"));

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
