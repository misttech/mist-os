// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/tests/fidl/server_suite/harness/harness.h"
#include "src/tests/fidl/server_suite/harness/ordinals.h"

using namespace channel_util;

namespace server_suite {

// Check that the test runner is set up correctly without doing anything else.
CLOSED_SERVER_TEST(Setup) {}

// Check that the test disabling mechanism works.
CLOSED_SERVER_TEST(IgnoreDisabled) {
  // This test will always fail when run - the only purpose of putting it here is to ensure that
  // each implementation's runner respects |!is_enabled()| tests by skipping over this code in all
  // cases.
  FAIL();
}

// The server should receive a one-way method request.
CLOSED_SERVER_TEST(OneWayNoPayload) {
  ASSERT_OK(client_end().write(
      header(kOneWayTxid, kOrdinalOneWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod)));

  WAIT_UNTIL([this]() { return reporter().received_one_way_no_payload(); });
}

// The server should reply to a two-way method request (no payload).
CLOSED_SERVER_TEST(TwoWayNoPayload) {
  ASSERT_OK(client_end().write(
      header(kTwoWayTxid, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod)));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  ASSERT_OK(client_end().read_and_check(
      header(kTwoWayTxid, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod)));
}

// The server should reply to a two-way method request (struct payload).
CLOSED_SERVER_TEST(TwoWayStructPayload) {
  Bytes bytes = {
      header(kTwoWayTxid, kOrdinalTwoWayStructPayload, fidl::MessageDynamicFlags::kStrictMethod),
      u8(kSomeByte),
      padding(7),
  };
  ASSERT_OK(client_end().write(bytes));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  ASSERT_OK(client_end().read_and_check(bytes));
}

// The server should reply to a two-way method request (table payload).
CLOSED_SERVER_TEST(TwoWayTablePayload) {
  Bytes bytes = {
      // clang-format off
    header(kTwoWayTxid, kOrdinalTwoWayTablePayload, fidl::MessageDynamicFlags::kStrictMethod),

    table_max_ordinal(1),
    pointer_present(),

    inline_envelope(u8(kSomeByte), false),
      // clang-format on
  };
  ASSERT_OK(client_end().write(bytes));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  ASSERT_OK(client_end().read_and_check(bytes));
}

// The server should reply to a two-way method request (union payload).
CLOSED_SERVER_TEST(TwoWayUnionPayload) {
  Bytes bytes = {
      // clang-format off
    header(kTwoWayTxid, kOrdinalTwoWayUnionPayload, fidl::MessageDynamicFlags::kStrictMethod),

    union_ordinal(1),
    inline_envelope(u8(kSomeByte), false),
      // clang-format on
  };
  ASSERT_OK(client_end().write(bytes));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  ASSERT_OK(client_end().read_and_check(bytes));
}

// The server should reply to a fallible method (success).
CLOSED_SERVER_TEST(TwoWayResultWithPayload) {
  Bytes bytes_in = {
      // clang-format off
      header(kTwoWayTxid, kOrdinalTwoWayResult, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(1), out_of_line_envelope(24, 0),
      string_header(3),
      'a','b','c', padding(5),
      // clang-format on
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      // clang-format off
      header(kTwoWayTxid, kOrdinalTwoWayResult, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(1), out_of_line_envelope(24, 0),
      string_header(3),
      'a','b','c', padding(5),
      // clang-format on
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a fallible method (error).
CLOSED_SERVER_TEST(TwoWayResultWithError) {
  Bytes bytes_in = {
      // clang-format off
      header(kTwoWayTxid, kOrdinalTwoWayResult, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(2), inline_envelope(u32(kSomeByte), false),
      // clang-format on
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      // clang-format off
      header(kTwoWayTxid, kOrdinalTwoWayResult, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(2), inline_envelope(u32(kSomeByte), false),
      // clang-format on
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

}  // namespace server_suite
