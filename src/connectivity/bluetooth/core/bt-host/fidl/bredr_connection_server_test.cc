// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bredr_connection_server.h"

#include <cstddef>

#include <gmock/gmock.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/l2cap/fake_channel.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/loop_fixture.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/test_helpers.h"

using FakeChannel = bt::l2cap::testing::FakeChannel;
using Channel = fuchsia::bluetooth::Channel;

namespace bthost {
namespace {

class BrEdrConnectionServerTest : public bt::testing::TestLoopFixture {
 public:
  BrEdrConnectionServerTest()
      : fake_chan_(/*id=*/1, /*remote_id=*/2, /*handle=*/3, bt::LinkType::kACL) {}

  FakeChannel& fake_chan() { return fake_chan_; }

 private:
  FakeChannel fake_chan_;
};

class BrEdrConnectionServerChannelActivatedTest : public BrEdrConnectionServerTest {
 public:
  void SetUp() override {
    BrEdrConnectionServerTest::SetUp();
    fidl::InterfaceHandle<Channel> handle;
    auto closed_cb = [this]() { server_closed_ = true; };
    server_ = BrEdrConnectionServer::Create(handle.NewRequest(), fake_chan().AsWeakPtr(),
                                            std::move(closed_cb));
    ASSERT_TRUE(server_);
    ASSERT_TRUE(fake_chan().activated());
    client_ = handle.Bind();
  }

  void TearDown() override {
    BrEdrConnectionServerTest::TearDown();
    if (client_.is_bound()) {
      client_.Unbind();
    }
    server_.reset();
    RunLoopUntilIdle();
  }

  fidl::InterfacePtr<Channel>& client() { return client_; }

  bool server_closed() const { return server_closed_; }

  void DestroyServer() { server_.reset(); }

 private:
  bool server_closed_ = false;
  std::unique_ptr<BrEdrConnectionServer> server_;
  fidl::InterfacePtr<Channel> client_;
};

TEST_F(BrEdrConnectionServerChannelActivatedTest, SendTwoPackets) {
  std::vector<bt::ByteBufferPtr> sent_packets;
  auto chan_send_cb = [&](bt::ByteBufferPtr buffer) { sent_packets.push_back(std::move(buffer)); };
  fake_chan().SetSendCallback(std::move(chan_send_cb));

  const std::vector<uint8_t> packet_0 = {0x00, 0x01, 0x03};
  int send_cb_count = 0;
  client()->Send(packet_0, [&](fuchsia::bluetooth::Channel_Send_Result result) {
    EXPECT_TRUE(result.is_response());
    send_cb_count++;
  });
  RunLoopUntilIdle();
  EXPECT_EQ(send_cb_count, 1);
  ASSERT_EQ(sent_packets.size(), 1u);
  EXPECT_THAT(*sent_packets[0], bt::BufferEq(packet_0));

  const std::vector<uint8_t> packet_1 = {0x04, 0x05, 0x06};
  client()->Send(packet_1, [&](fuchsia::bluetooth::Channel_Send_Result result) {
    EXPECT_TRUE(result.is_response());
    send_cb_count++;
  });
  RunLoopUntilIdle();
  EXPECT_EQ(send_cb_count, 2);
  ASSERT_EQ(sent_packets.size(), 2u);
  EXPECT_THAT(*sent_packets[1], bt::BufferEq(packet_1));
}

TEST_F(BrEdrConnectionServerChannelActivatedTest, SendTooLargePacketDropsPacket) {
  std::vector<bt::ByteBufferPtr> sent_packets;
  auto chan_send_cb = [&](bt::ByteBufferPtr buffer) { sent_packets.push_back(std::move(buffer)); };
  fake_chan().SetSendCallback(std::move(chan_send_cb));

  const std::vector<uint8_t> packet_0(/*count=*/bt::l2cap::kDefaultMTU + 1, /*value=*/0x03);
  int send_cb_count = 0;
  client()->Send(packet_0, [&](fuchsia::bluetooth::Channel_Send_Result result) {
    EXPECT_TRUE(result.is_response());
    send_cb_count++;
  });
  RunLoopUntilIdle();
  EXPECT_EQ(send_cb_count, 1);
  ASSERT_EQ(sent_packets.size(), 0u);
}

TEST_F(BrEdrConnectionServerChannelActivatedTest,
       ReceiveManyPacketsAndDropSomeAndWaitForAcksToSendMore) {
  std::vector<std::vector<uint8_t>> packets;
  client().events().OnReceive = [&](std::vector<uint8_t> packet) {
    packets.push_back(std::move(packet));
  };

  for (uint8_t i = 0; i < 2 * BrEdrConnectionServer::kDefaultReceiveCredits; i++) {
    bt::StaticByteBuffer packet(i, 0x01, 0x02);
    fake_chan().Receive(packet);
  }

  RunLoopUntilIdle();
  ASSERT_EQ(packets.size(), BrEdrConnectionServer::kDefaultReceiveCredits);
  for (uint8_t i = 0; i < BrEdrConnectionServer::kDefaultReceiveCredits; i++) {
    bt::StaticByteBuffer packet(i, 0x01, 0x02);
    EXPECT_THAT(packets[i], bt::BufferEq(packet));
  }

  // Some packets were dropped, so only the packets under the queue limit should be received.
  size_t first_packet_in_q =
      static_cast<size_t>(2 * BrEdrConnectionServer::kDefaultReceiveCredits) -
      BrEdrConnectionServer::kDefaultReceiveQueueLimit;
  for (size_t i = 0; i < BrEdrConnectionServer::kDefaultReceiveQueueLimit; i++) {
    client()->AckReceive();
    RunLoopUntilIdle();
    ASSERT_EQ(packets.size(), BrEdrConnectionServer::kDefaultReceiveCredits + i + 1);
    bt::StaticByteBuffer packet(first_packet_in_q + i, 0x01, 0x02);
    EXPECT_THAT(packets[BrEdrConnectionServer::kDefaultReceiveCredits + i], bt::BufferEq(packet));
  }
}

TEST_F(BrEdrConnectionServerChannelActivatedTest, ChannelCloses) {
  std::optional<zx_status_t> error;
  client().set_error_handler([&](zx_status_t status) { error = status; });
  fake_chan().Close();
  EXPECT_TRUE(server_closed());
  RunLoopUntilIdle();
  ASSERT_TRUE(error.has_value());
  EXPECT_EQ(error.value(), ZX_ERR_CONNECTION_RESET);
}

TEST_F(BrEdrConnectionServerChannelActivatedTest, ClientCloses) {
  client().Unbind();
  RunLoopUntilIdle();
  EXPECT_TRUE(server_closed());
  EXPECT_FALSE(fake_chan().activated());
}

TEST_F(BrEdrConnectionServerTest, ActivateFails) {
  fake_chan().set_activate_fails(true);
  fidl::InterfaceHandle<Channel> handle;
  bool server_closed = false;
  auto closed_cb = [&]() { server_closed = true; };
  std::unique_ptr<BrEdrConnectionServer> server = BrEdrConnectionServer::Create(
      handle.NewRequest(), fake_chan().AsWeakPtr(), std::move(closed_cb));
  EXPECT_FALSE(server);
  EXPECT_FALSE(server_closed);
}

TEST_F(BrEdrConnectionServerChannelActivatedTest, DeactivateOnServerDestruction) {
  EXPECT_TRUE(fake_chan().activated());
  DestroyServer();
  EXPECT_FALSE(fake_chan().activated());
}

}  // namespace
}  // namespace bthost
