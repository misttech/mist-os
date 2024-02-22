// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/basic_mode_tx_engine.h"

#include <gtest/gtest.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/fake_tx_channel.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/test_helpers.h"

namespace bt::l2cap::internal {
namespace {

constexpr ChannelId kTestChannelId = 0x0001;

TEST(BasicModeTxEngineTest, ProcessSduTransmitsMinimalSizedSdu) {
  ByteBufferPtr last_pdu;
  size_t n_pdus = 0;
  FakeTxChannel channel;
  channel.HandleSendFrame([&](auto pdu) {
    ++n_pdus;
    last_pdu = std::move(pdu);
  });

  constexpr size_t kMtu = 10;
  const StaticByteBuffer payload(1);
  channel.QueueSdu(std::make_unique<DynamicByteBuffer>(payload));
  BasicModeTxEngine(kTestChannelId, kMtu, channel).NotifySduQueued();
  EXPECT_EQ(1u, n_pdus);
  ASSERT_TRUE(last_pdu);
  EXPECT_TRUE(ContainersEqual(payload, *last_pdu));
}

TEST(BasicModeTxEngineTest, ProcessSduTransmitsMaximalSizedSdu) {
  ByteBufferPtr last_pdu;
  size_t n_pdus = 0;
  FakeTxChannel channel;
  channel.HandleSendFrame([&](auto pdu) {
    ++n_pdus;
    last_pdu = std::move(pdu);
  });

  constexpr size_t kMtu = 1;
  const StaticByteBuffer payload(1);
  channel.QueueSdu(std::make_unique<DynamicByteBuffer>(payload));
  BasicModeTxEngine(kTestChannelId, kMtu, channel).NotifySduQueued();
  EXPECT_EQ(1u, n_pdus);
  ASSERT_TRUE(last_pdu);
  EXPECT_TRUE(ContainersEqual(payload, *last_pdu));
}

TEST(BasicModeTxEngineTest, ProcessSduDropsOversizedSdu) {
  FakeTxChannel channel;
  size_t n_pdus = 0;
  channel.HandleSendFrame([&](auto pdu) { ++n_pdus; });

  constexpr size_t kMtu = 1;
  channel.QueueSdu(std::make_unique<DynamicByteBuffer>(StaticByteBuffer(1, 2)));
  BasicModeTxEngine(kTestChannelId, kMtu, channel).NotifySduQueued();
  EXPECT_EQ(0u, n_pdus);
}

TEST(BasicModeTxEngineTest, ProcessSduSurvivesZeroByteSdu) {
  FakeTxChannel channel;
  size_t n_pdus = 0;
  channel.HandleSendFrame([&](auto pdu) { ++n_pdus; });
  constexpr size_t kMtu = 1;
  channel.QueueSdu(std::make_unique<DynamicByteBuffer>());
  BasicModeTxEngine(kTestChannelId, kMtu, channel).NotifySduQueued();
  EXPECT_EQ(1u, n_pdus);
}

}  // namespace
}  // namespace bt::l2cap::internal
