// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/types.h>

#include "src/tests/fidl/server_suite/harness/harness.h"
#include "src/tests/fidl/server_suite/harness/ordinals.h"

using namespace channel_util;

namespace server_suite {

// The server should tear down when it receives a one-way request with nonzero txid.
CLOSED_SERVER_TEST(OneWayWithNonZeroTxid) {
  ASSERT_OK(client_end().write(header(56 /* txid not 0 */, kOrdinalOneWayNoPayload,
                                      fidl::MessageDynamicFlags::kStrictMethod)));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  WAIT_UNTIL([this] { return reporter().teardown_reason().has_value(); });
  EXPECT_TEARDOWN_REASON(fidl_serversuite::TeardownReason::kUnexpectedMessage);
}

// The server should tear down when it receives a two-way request with zero txid.
CLOSED_SERVER_TEST(TwoWayNoPayloadWithZeroTxid) {
  ASSERT_OK(client_end().write(
      header(0, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod)));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  WAIT_UNTIL([this] { return reporter().teardown_reason().has_value(); });
  EXPECT_TEARDOWN_REASON(fidl_serversuite::TeardownReason::kUnexpectedMessage);
}

// The closed server should tear down when it receives a request with an unknown ordinal.
CLOSED_SERVER_TEST(UnknownOrdinalCausesClose) {
  ASSERT_OK(client_end().write(
      header(0, /* some wrong ordinal */ 8888888lu, fidl::MessageDynamicFlags::kStrictMethod)));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  WAIT_UNTIL([this] { return reporter().teardown_reason().has_value(); });
  EXPECT_TEARDOWN_REASON(fidl_serversuite::TeardownReason::kUnexpectedMessage);
}

// The server should tear down when it receives a request with an invalid magic number.
CLOSED_SERVER_TEST(BadMagicNumberCausesClose) {
  ASSERT_OK(client_end().write(as_bytes(fidl_message_header_t{
      .txid = kTwoWayTxid,
      .at_rest_flags = {FIDL_MESSAGE_HEADER_AT_REST_FLAGS_0_USE_VERSION_V2, 0},
      .dynamic_flags = FIDL_MESSAGE_HEADER_DYNAMIC_FLAGS_STRICT_METHOD,
      .magic_number = 0xff,  // Chosen to be invalid
      .ordinal = kOrdinalTwoWayNoPayload,
  })));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  WAIT_UNTIL([this] { return reporter().teardown_reason().has_value(); });
  EXPECT_TEARDOWN_REASON(fidl_serversuite::TeardownReason::kDecodingError);
}

// The server should ignore unrecognized at-rest flags.
CLOSED_SERVER_TEST(IgnoresUnrecognizedAtRestFlags) {
  Bytes bytes_in = {
      as_bytes(fidl_message_header_t{
          .txid = kTwoWayTxid,
          .at_rest_flags = {FIDL_MESSAGE_HEADER_AT_REST_FLAGS_0_USE_VERSION_V2 | 100, 200},
          .dynamic_flags = FIDL_MESSAGE_HEADER_DYNAMIC_FLAGS_STRICT_METHOD,
          .magic_number = kFidlWireFormatMagicNumberInitial,
          .ordinal = kOrdinalTwoWayNoPayload,
      }),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should ignore unrecognized dynamic flags.
CLOSED_SERVER_TEST(IgnoresUnrecognizedDynamicFlags) {
  Bytes bytes_in = {
      as_bytes(fidl_message_header_t{
          .txid = kTwoWayTxid,
          .at_rest_flags = {FIDL_MESSAGE_HEADER_AT_REST_FLAGS_0_USE_VERSION_V2, 0},
          .dynamic_flags = 7,  // First 3 bits are flipped.
          .magic_number = kFidlWireFormatMagicNumberInitial,
          .ordinal = kOrdinalTwoWayNoPayload,
      }),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

}  // namespace server_suite
