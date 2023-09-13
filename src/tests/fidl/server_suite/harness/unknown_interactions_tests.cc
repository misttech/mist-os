// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.serversuite/cpp/common_types.h>
#include <fidl/fidl.serversuite/cpp/natural_types.h>
#include <lib/zx/eventpair.h>
#include <zircon/fidl.h>

#include "src/lib/testing/predicates/status.h"
#include "src/tests/fidl/channel_util/bytes.h"
#include "src/tests/fidl/channel_util/channel.h"
#include "src/tests/fidl/server_suite/harness/harness.h"
#include "src/tests/fidl/server_suite/harness/ordinals.h"

namespace server_suite {
namespace {

using namespace ::channel_util;

// The server should send a strict event.
OPEN_SERVER_TEST(SendStrictEvent) {
  Bytes expected_event = Header{.txid = 0, .ordinal = kOrdinalStrictEvent};
  controller()->SendStrictEvent().ThenExactlyOnce([&](auto result) {
    MarkControllerCallbackRun();
    ASSERT_TRUE(result.is_ok());
  });
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(expected_event));
  WAIT_UNTIL_CONTROLLER_CALLBACK_RUN();
}

// The server should send a flexible event.
OPEN_SERVER_TEST(SendFlexibleEvent) {
  Bytes expected_event = Header{
      .txid = 0,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalFlexibleEvent,
  };
  controller()->SendFlexibleEvent().ThenExactlyOnce([&](auto result) {
    MarkControllerCallbackRun();
    ASSERT_TRUE(result.is_ok());
  });
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(expected_event));
  WAIT_UNTIL_CONTROLLER_CALLBACK_RUN();
}

// The server should receive a strict one-way method.
OPEN_SERVER_TEST(ReceiveStrictOneWay) {
  Bytes request = Header{.txid = 0, .ordinal = kOrdinalStrictOneWay};
  ASSERT_OK(client_end().write(request));
  WAIT_UNTIL([this]() { return reporter().received_strict_one_way(); });
  ;
}

// The server should receive a one-way method, despite the schema (strict)
// not matching the dynamic flags (flexible).
OPEN_SERVER_TEST(ReceiveStrictOneWayMismatchedStrictness) {
  Bytes request = Header{
      .txid = 0,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalStrictOneWay,
  };
  ASSERT_OK(client_end().write(request));
  WAIT_UNTIL([this]() { return reporter().received_strict_one_way(); });
  ;
}

// The server should receive a flexible one-way method.
OPEN_SERVER_TEST(ReceiveFlexibleOneWay) {
  Bytes request = Header{
      .txid = 0,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalFlexibleOneWay,
  };
  ASSERT_OK(client_end().write(request));
  WAIT_UNTIL([this]() { return reporter().received_flexible_one_way(); });
  ;
}

// The server should receive a one-way method, despite the schema (flexible)
// not matching the dynamic flags (strict).
OPEN_SERVER_TEST(ReceiveFlexibleOneWayMismatchedStrictness) {
  Bytes request = Header{.txid = 0, .ordinal = kOrdinalFlexibleOneWay};
  ASSERT_OK(client_end().write(request));
  WAIT_UNTIL([this]() { return reporter().received_flexible_one_way(); });
}

// The server should reply to a strict two-way method.
OPEN_SERVER_TEST(StrictTwoWayResponse) {
  Bytes bytes = Header{.txid = kTwoWayTxid, .ordinal = kOrdinalStrictTwoWay};
  ASSERT_OK(client_end().write(bytes));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(bytes));
}

// The server should reply to a two-way method, despite the schema (strict)
// not matching the request's dynamic flags (flexible).
OPEN_SERVER_TEST(StrictTwoWayResponseMismatchedStrictness) {
  Bytes request = Header{
      .txid = kTwoWayTxid,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalStrictTwoWay,
  };
  Bytes expected_response = Header{
      .txid = kTwoWayTxid,
      .ordinal = kOrdinalStrictTwoWay,
  };
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(expected_response));
}

// The server should reply to a strict two-way method (nonempty).
OPEN_SERVER_TEST(StrictTwoWayNonEmptyResponse) {
  Bytes bytes = {
      Header{.txid = kTwoWayTxid, .ordinal = kOrdinalStrictTwoWayFields},
      encode(fidl_serversuite::OpenTargetStrictTwoWayFieldsRequest(504230)),
  };
  ASSERT_OK(client_end().write(bytes));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(bytes));
}

// The server should reply to a strict fallible two-way method (success).
OPEN_SERVER_TEST(StrictTwoWayErrorSyntaxResponse) {
  Bytes bytes = {
      Header{.txid = kTwoWayTxid, .ordinal = kOrdinalStrictTwoWayErr},
      union_ordinal(kResultUnionSuccess),
      inline_envelope({0x00}),
  };
  ASSERT_OK(client_end().write(bytes));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(bytes));
}

// The server should reply to a fallible two-way method (success), despite the
// schema (strict) not matching the request's dynamic flags (flexible).
OPEN_SERVER_TEST(StrictTwoWayErrorSyntaxResponseMismatchedStrictness) {
  Bytes payload = {
      union_ordinal(kResultUnionSuccess),
      inline_envelope({0x00}),
  };
  Bytes request = {
      Header{
          .txid = kTwoWayTxid,
          .dynamic_flags = kDynamicFlagsFlexible,
          .ordinal = kOrdinalStrictTwoWayErr,
      },
      payload,
  };
  Bytes expected_response = {
      Header{
          .txid = kTwoWayTxid,
          .ordinal = kOrdinalStrictTwoWayErr,
      },
      payload,
  };
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(expected_response));
}

// The server should reply to a strict fallible two-way method (nonempty success).
OPEN_SERVER_TEST(StrictTwoWayErrorSyntaxNonEmptyResponse) {
  Bytes bytes = {
      Header{.txid = kTwoWayTxid, .ordinal = kOrdinalStrictTwoWayFieldsErr},
      union_ordinal(kResultUnionSuccess),
      inline_envelope(int32(406601)),
  };
  ASSERT_OK(client_end().write(bytes));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(bytes));
}

// The server should reply to a flexible two-way method.
OPEN_SERVER_TEST(FlexibleTwoWayResponse) {
  Header header = {
      .txid = kTwoWayTxid,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalFlexibleTwoWay,
  };
  Bytes request = header;
  Bytes expected_response = {header, union_ordinal(kResultUnionSuccess), inline_envelope({0x00})};
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(expected_response));
}

// The server should reply to a two-way method, despite the schema (flexible)
// not matching the request's dynamic flags (strict).
OPEN_SERVER_TEST(FlexibleTwoWayResponseMismatchedStrictness) {
  Bytes request = Header{
      .txid = kTwoWayTxid,
      .ordinal = kOrdinalFlexibleTwoWay,
  };
  Bytes expected_response = {
      Header{
          .txid = kTwoWayTxid,
          .dynamic_flags = kDynamicFlagsFlexible,
          .ordinal = kOrdinalFlexibleTwoWay,
      },
      union_ordinal(kResultUnionSuccess),
      inline_envelope({0x00}),
  };
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(expected_response));
}

// The server should reply to a flexible two-way method (nonempty).
OPEN_SERVER_TEST(FlexibleTwoWayNonEmptyResponse) {
  Header header = {
      .txid = kTwoWayTxid,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalFlexibleTwoWayFields,
  };
  Bytes payload = int32(3023950);
  Bytes request = {header, payload, padding(4)};
  Bytes expected_response = {header, union_ordinal(kResultUnionSuccess), inline_envelope(payload)};
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(expected_response));
}

// The server should reply to a flexible fallible two-way method (success).
OPEN_SERVER_TEST(FlexibleTwoWayErrorSyntaxResponseSuccessResult) {
  Bytes bytes = {
      Header{
          .txid = kTwoWayTxid,
          .dynamic_flags = kDynamicFlagsFlexible,
          .ordinal = kOrdinalFlexibleTwoWayErr,
      },
      union_ordinal(kResultUnionSuccess),
      inline_envelope({0x00}),
  };
  ASSERT_OK(client_end().write(bytes));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(bytes));
}

// The server should reply to a flexible fallible two-way method (error).
OPEN_SERVER_TEST(FlexibleTwoWayErrorSyntaxResponseErrorResult) {
  Bytes bytes = {
      Header{
          .txid = kTwoWayTxid,
          .dynamic_flags = kDynamicFlagsFlexible,
          .ordinal = kOrdinalFlexibleTwoWayErr,
      },
      union_ordinal(kResultUnionDomainError),
      inline_envelope(int32(60602293)),
  };
  ASSERT_OK(client_end().write(bytes));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(bytes));
}

// The server should reply to a flexible fallible two-way method (nonempty success).
OPEN_SERVER_TEST(FlexibleTwoWayErrorSyntaxNonEmptyResponseSuccessResult) {
  Bytes bytes = {
      Header{
          .txid = kTwoWayTxid,
          .dynamic_flags = kDynamicFlagsFlexible,
          .ordinal = kOrdinalFlexibleTwoWayFieldsErr,
      },
      union_ordinal(kResultUnionSuccess),
      inline_envelope(int32(406601)),
  };
  ASSERT_OK(client_end().write(bytes));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(bytes));
}

// The server should reply to a flexible fallible two-way method (nonempty, error).
OPEN_SERVER_TEST(FlexibleTwoWayErrorSyntaxNonEmptyResponseErrorResult) {
  Bytes bytes = {
      Header{
          .txid = kTwoWayTxid,
          .dynamic_flags = kDynamicFlagsFlexible,
          .ordinal = kOrdinalFlexibleTwoWayFieldsErr,
      },
      union_ordinal(kResultUnionDomainError),
      inline_envelope(int32(60602293)),
  };
  ASSERT_OK(client_end().write(bytes));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(bytes));
}

// The server should tear down when it receives an unknown strict one-way method.
OPEN_SERVER_TEST(UnknownStrictOneWayOpenProtocol) {
  Bytes request = Header{.txid = 0, .ordinal = kOrdinalFakeUnknownMethod};
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  ASSERT_FALSE(reporter().received_unknown_method());
}

// The server should run the unknown method handler when it receives an unknown
// flexible one-way method.
OPEN_SERVER_TEST(UnknownFlexibleOneWayOpenProtocol) {
  Bytes request = Header{
      .txid = 0,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalFakeUnknownMethod,
  };
  ASSERT_OK(client_end().write(request));
  WAIT_UNTIL([this]() { return reporter().received_unknown_method().has_value(); });
  ASSERT_EQ(kOrdinalFakeUnknownMethod, reporter().received_unknown_method()->ordinal());
  ASSERT_EQ(fidl_serversuite::UnknownMethodType::kOneWay,
            reporter().received_unknown_method()->unknown_method_type());
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The server should close handles in an unknown flexible one-way method request.
OPEN_SERVER_TEST(UnknownFlexibleOneWayHandleOpenProtocol) {
  zx::eventpair event1, event2;
  ASSERT_OK(zx::eventpair::create(0, &event1, &event2));

  Message request = {
      Bytes{
          Header{
              .txid = 0,
              .dynamic_flags = kDynamicFlagsFlexible,
              .ordinal = kOrdinalFakeUnknownMethod,
          },
          {handle_present(), padding(4)},
      },
      Handles{
          {.handle = event1.release(), .type = ZX_OBJ_TYPE_EVENTPAIR},
      },
  };
  ASSERT_OK(client_end().write(request));
  WAIT_UNTIL([this]() { return reporter().received_unknown_method().has_value(); });
  ASSERT_EQ(kOrdinalFakeUnknownMethod, reporter().received_unknown_method()->ordinal());
  ASSERT_EQ(fidl_serversuite::UnknownMethodType::kOneWay,
            reporter().received_unknown_method()->unknown_method_type());
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_OK(event2.wait_one(ZX_EVENTPAIR_PEER_CLOSED, zx::time::infinite_past(), nullptr));
}

// The server should tear down when it receives an unknown strict two-way method.
OPEN_SERVER_TEST(UnknownStrictTwoWayOpenProtocol) {
  Bytes request = Header{.txid = kTwoWayTxid, .ordinal = kOrdinalFakeUnknownMethod};
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  ASSERT_FALSE(reporter().received_unknown_method());
}

// The server should send an automatic reply and run the unknown method handler
// when it receives an unknown flexible two-way method.
OPEN_SERVER_TEST(UnknownFlexibleTwoWayOpenProtocol) {
  Header header = {
      .txid = kTwoWayTxid,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalFakeUnknownMethod,
  };
  Bytes request = header;
  Bytes expected_response = {
      header,
      union_ordinal(kResultUnionFrameworkError),
      inline_envelope(framework_err_unknown_method()),
  };
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(expected_response));
  WAIT_UNTIL([this]() { return reporter().received_unknown_method().has_value(); });
  ASSERT_EQ(kOrdinalFakeUnknownMethod, reporter().received_unknown_method()->ordinal());
  ASSERT_EQ(fidl_serversuite::UnknownMethodType::kTwoWay,
            reporter().received_unknown_method()->unknown_method_type());
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The server should close handles in an unknown flexible two-way method request.
OPEN_SERVER_TEST(UnknownFlexibleTwoWayHandleOpenProtocol) {
  zx::eventpair event1, event2;
  ASSERT_OK(zx::eventpair::create(0, &event1, &event2));

  Header header = {
      .txid = kTwoWayTxid,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalFakeUnknownMethod,
  };
  Message request = {
      Bytes{header, handle_present(), padding(4)},
      Handles{{.handle = event1.release(), .type = ZX_OBJ_TYPE_EVENTPAIR}},
  };
  ExpectedMessage expected_response = {
      Bytes{
          header,
          union_ordinal(kResultUnionFrameworkError),
          inline_envelope(framework_err_unknown_method()),
      },
      ExpectedHandles{},
  };
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(expected_response));
  WAIT_UNTIL([this]() { return reporter().received_unknown_method().has_value(); });
  ASSERT_EQ(kOrdinalFakeUnknownMethod, reporter().received_unknown_method()->ordinal());
  ASSERT_EQ(fidl_serversuite::UnknownMethodType::kTwoWay,
            reporter().received_unknown_method()->unknown_method_type());
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_OK(event2.wait_one(ZX_EVENTPAIR_PEER_CLOSED, zx::time::infinite_past(), nullptr));
}

// The ajar server should tear down when it receives an unknown strict one-way method.
AJAR_SERVER_TEST(UnknownStrictOneWayAjarProtocol) {
  Bytes request = Header{.txid = 0, .ordinal = kOrdinalFakeUnknownMethod};
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  ASSERT_FALSE(reporter().received_unknown_method());
}

// The ajar server should run the unknown method handler when it receives an unknown
// flexible one-way method.
AJAR_SERVER_TEST(UnknownFlexibleOneWayAjarProtocol) {
  Bytes request = Header{
      .txid = 0,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalFakeUnknownMethod,
  };
  ASSERT_OK(client_end().write(request));
  WAIT_UNTIL([this]() { return reporter().received_unknown_method().has_value(); });
  ASSERT_EQ(kOrdinalFakeUnknownMethod, reporter().received_unknown_method()->ordinal());
  ASSERT_EQ(fidl_serversuite::UnknownMethodType::kOneWay,
            reporter().received_unknown_method()->unknown_method_type());
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The ajar server should tear down when it receives an unknown strict two-way method.
AJAR_SERVER_TEST(UnknownStrictTwoWayAjarProtocol) {
  Bytes request = Header{.txid = kTwoWayTxid, .ordinal = kOrdinalFakeUnknownMethod};
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  ASSERT_FALSE(reporter().received_unknown_method());
}

// The ajar server should tear down when it receives an unknown flexible two-way method.
AJAR_SERVER_TEST(UnknownFlexibleTwoWayAjarProtocol) {
  Bytes request = Header{
      .txid = kTwoWayTxid,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalFakeUnknownMethod,
  };
  ASSERT_OK(client_end().write(request));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  ASSERT_FALSE(reporter().received_unknown_method());
}

// The closed server should tear down when it receives an unknown strict one-way method.
CLOSED_SERVER_TEST(UnknownStrictOneWayClosedProtocol) {
  Bytes request = Header{.txid = 0, .ordinal = kOrdinalFakeUnknownMethod};
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

// The closed server should tear down when it receives an unknown flexible one-way method.
CLOSED_SERVER_TEST(UnknownFlexibleOneWayClosedProtocol) {
  Bytes request = Header{
      .txid = 0,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalFakeUnknownMethod,
  };
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

// The closed server should tear down when it receives an unknown strict two-way method.
CLOSED_SERVER_TEST(UnknownStrictTwoWayClosedProtocol) {
  Bytes request = Header{.txid = kTwoWayTxid, .ordinal = kOrdinalFakeUnknownMethod};
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

// The closed server should tear down when it receives an unknown flexible two-way method.
CLOSED_SERVER_TEST(UnknownFlexibleTwoWayClosedProtocol) {
  Bytes request = Header{
      .txid = kTwoWayTxid,
      .dynamic_flags = kDynamicFlagsFlexible,
      .ordinal = kOrdinalFakeUnknownMethod,
  };
  ASSERT_OK(client_end().write(request));
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

}  // namespace
}  // namespace server_suite
