// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/tests/fidl/server_suite/harness/harness.h"
#include "src/tests/fidl/server_suite/harness/ordinals.h"

using namespace channel_util;

namespace server_suite {

OPEN_SERVER_TEST(EventSendingDoNotReportPeerClosed) {
  client_end().reset();

  controller()->SendStrictEvent().ThenExactlyOnce([&](auto result) {
    MarkControllerCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
  });

  WAIT_UNTIL([this] { return reporter().teardown_reason().has_value(); });
  EXPECT_TEARDOWN_REASON(fidl_serversuite::TeardownReason::kChannelPeerClosed);
}

CLOSED_SERVER_TEST(ReplySendingDoNotReportPeerClosed) {
  ASSERT_OK(client_end().write(
      header(kTwoWayTxid, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod)));

  client_end().reset();

  WAIT_UNTIL([this] { return reporter().teardown_reason().has_value(); });
  EXPECT_TEARDOWN_REASON(fidl_serversuite::TeardownReason::kChannelPeerClosed);
}

// The server should drain out messages buffered by a client, even though the
// client closed their endpoint right away after writing those messages.
CLOSED_SERVER_TEST(ReceiveOneWayNoPayloadFromPeerClosedChannel) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalOneWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));
  client_end().reset();

  WAIT_UNTIL([this] { return reporter().received_one_way_no_payload(); });
}

CLOSED_SERVER_TEST(ServerTearsDownWhenPeerClosed) {
  client_end().reset();
  WAIT_UNTIL([this] { return reporter().teardown_reason().has_value(); });
  EXPECT_TEARDOWN_REASON(fidl_serversuite::TeardownReason::kChannelPeerClosed);
}

}  // namespace server_suite
