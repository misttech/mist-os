// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/tests/fidl/server_suite/harness/harness.h"
#include "src/tests/fidl/server_suite/harness/ordinals.h"

using namespace channel_util;

namespace server_suite {

// When sending an event, if channel_write returns PEER_CLOSED, the bindings
// should hide it and return successfully. This helps prevent race conditions.
OPEN_SERVER_TEST(EventSendingDoNotReportPeerClosed) {
  client_end().reset();

  controller()->SendStrictEvent().ThenExactlyOnce([&](auto result) {
    MarkControllerCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
  });

  WAIT_UNTIL([this] { return reporter().teardown_reason().has_value(); });
  EXPECT_TEARDOWN_REASON(fidl_serversuite::TeardownReason::kChannelPeerClosed);
  WAIT_UNTIL_CONTROLLER_CALLBACK_RUN();
}

// When sending a reply, if channel_write returns PEER_CLOSED, the bindings
// should hide it and return successfully. This helps prevent race conditions.
CLOSED_SERVER_TEST(ReplySendingDoNotReportPeerClosed) {
  ASSERT_OK(client_end().write(
      header(kTwoWayTxid, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod)));

  client_end().reset();

  WAIT_UNTIL([this] { return reporter().teardown_reason().has_value(); });
  EXPECT_TEARDOWN_REASON(fidl_serversuite::TeardownReason::kChannelPeerClosed);
}

// The server should drain out messages buffered by a client, even when the
// client closed their endpoint right away after writing those messages.
CLOSED_SERVER_TEST(ReceiveOneWayNoPayloadFromPeerClosedChannel) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalOneWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));
  client_end().reset();

  WAIT_UNTIL([this] { return reporter().received_one_way_no_payload(); });
}

// This test isn't really necessary, since the test fixture does this implicitly
// at the end of all tests that don't call ASSERT_SERVER_TEARDOWN themselves.
// We include it here just to be explicit that this behavior is covered.
CLOSED_SERVER_TEST(ServerTearsDownWhenPeerClosed) {
  client_end().reset();
  WAIT_UNTIL([this] { return reporter().teardown_reason().has_value(); });
  EXPECT_TEARDOWN_REASON(fidl_serversuite::TeardownReason::kChannelPeerClosed);
}

}  // namespace server_suite
