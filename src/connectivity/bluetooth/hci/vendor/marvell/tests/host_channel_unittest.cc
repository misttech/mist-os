// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/vendor/marvell/host_channel.h"

#include <zxtest/zxtest.h>

namespace bt_hci_marvell {

class HostChannelTest : public zxtest::Test {};

// Not much to do here, just verify that we can create a host channel and retrieve the values we
// expect.
TEST_F(HostChannelTest, BasicOperation) {
  zx::channel ch;
  zx::unowned<zx::channel> ch_dup = ch.borrow();
  HostChannel host_channel(std::move(ch), ControllerChannelId::kChannelAclData,
                           ControllerChannelId::kChannelVendor, /* interrupt_key */ 0x1234,
                           "random");
  EXPECT_EQ(host_channel.channel(), *ch_dup);
  EXPECT_EQ(host_channel.read_id(), ControllerChannelId::kChannelAclData);
  EXPECT_EQ(host_channel.write_id(), ControllerChannelId::kChannelVendor);
  EXPECT_EQ(strcmp(host_channel.name(), "random"), 0);
}

}  // namespace bt_hci_marvell
