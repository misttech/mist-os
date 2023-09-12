// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.serversuite/cpp/natural_types.h>
#include <lib/zx/eventpair.h>

#include "src/tests/fidl/server_suite/harness/harness.h"
#include "src/tests/fidl/server_suite/harness/ordinals.h"

namespace server_suite {
namespace {

using namespace ::channel_util;

const fidl_xunion_tag_t kResultUnionSuccess = 1;
const fidl_xunion_tag_t kResultUnionError = 2;
const fidl_xunion_tag_t kResultUnionTransportError = 3;

// The server should send a strict event.
OPEN_SERVER_TEST(SendStrictEvent) {
  controller()->SendStrictEvent().ThenExactlyOnce([&](auto result) {
    MarkControllerCallbackRun();
    ASSERT_TRUE(result.is_ok());
  });

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kOneWayTxid, kOrdinalStrictEvent, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));

  WAIT_UNTIL_CONTROLLER_CALLBACK_RUN();
}

// The server should send a flexible event.
OPEN_SERVER_TEST(SendFlexibleEvent) {
  controller()->SendFlexibleEvent().ThenExactlyOnce([&](auto result) {
    MarkControllerCallbackRun();
    ASSERT_TRUE(result.is_ok());
  });

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kOneWayTxid, kOrdinalFlexibleEvent, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));

  WAIT_UNTIL_CONTROLLER_CALLBACK_RUN();
}

// The server should receive a strict one-way method.
OPEN_SERVER_TEST(ReceiveStrictOneWay) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalStrictOneWay, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  WAIT_UNTIL([this]() { return reporter().received_strict_one_way(); });
}

// The server should receive a one-way method, despite the schema (strict)
// not matching the dynamic flags (flexible).
OPEN_SERVER_TEST(ReceiveStrictOneWayMismatchedStrictness) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalStrictOneWay, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  WAIT_UNTIL([this]() { return reporter().received_strict_one_way(); });
}

// The server should receive a flexible one-way method.
OPEN_SERVER_TEST(ReceiveFlexibleOneWay) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFlexibleOneWay, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  WAIT_UNTIL([this]() { return reporter().received_flexible_one_way(); });
}

// The server should receive a one-way method, despite the schema (flexible)
// not matching the dynamic flags (strict).
OPEN_SERVER_TEST(ReceiveFlexibleOneWayMismatchedStrictness) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFlexibleOneWay, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  WAIT_UNTIL([this]() { return reporter().received_flexible_one_way(); });
}

// The server should reply to a strict two-way method.
OPEN_SERVER_TEST(StrictTwoWayResponse) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalStrictTwoWay, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalStrictTwoWay, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a two-way method, despite the schema (strict)
// not matching the request's dynamic flags (flexible).
OPEN_SERVER_TEST(StrictTwoWayResponseMismatchedStrictness) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalStrictTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalStrictTwoWay, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a strict two-way method (nonempty).
OPEN_SERVER_TEST(StrictTwoWayNonEmptyResponse) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalStrictTwoWayFields, fidl::MessageDynamicFlags::kStrictMethod),
      encode(fidl_serversuite::OpenTargetStrictTwoWayFieldsRequest(504230)),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalStrictTwoWayFields, fidl::MessageDynamicFlags::kStrictMethod),
      encode(fidl_serversuite::OpenTargetStrictTwoWayFieldsResponse(504230)),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a strict fallible two-way method (success).
OPEN_SERVER_TEST(StrictTwoWayErrorSyntaxResponse) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kStrictMethod),
      encode(fidl_serversuite::OpenTargetStrictTwoWayErrRequest::WithReplySuccess({})),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope(
          {
              padding(4),
          },
          false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a fallible two-way method (success), despite the
// schema (strict) not matching the request's dynamic flags (flexible).
OPEN_SERVER_TEST(StrictTwoWayErrorSyntaxResponseMismatchedStrictness) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      encode(fidl_serversuite::OpenTargetStrictTwoWayErrRequest::WithReplySuccess({})),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope(
          {
              padding(4),
          },
          false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a strict fallible two-way method (nonempty success).
OPEN_SERVER_TEST(StrictTwoWayErrorSyntaxNonEmptyResponse) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalStrictTwoWayFieldsErr, fidl::MessageDynamicFlags::kStrictMethod),
      encode(fidl_serversuite::OpenTargetStrictTwoWayFieldsErrRequest::WithReplySuccess(406601)),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalStrictTwoWayFieldsErr, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({i32(406601)}, false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a flexible two-way method.
OPEN_SERVER_TEST(FlexibleTwoWayResponse) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({padding(4)}, false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a two-way method, despite the schema (flexible)
// not matching the request's dynamic flags (strict).
OPEN_SERVER_TEST(FlexibleTwoWayResponseMismatchedStrictness) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({padding(4)}, false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a flexible two-way method (nonempty).
OPEN_SERVER_TEST(FlexibleTwoWayNonEmptyResponse) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWayFields, fidl::MessageDynamicFlags::kFlexibleMethod),
      encode(fidl_serversuite::OpenTargetFlexibleTwoWayFieldsRequest(3023950)),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWayFields, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({i32(3023950)}, false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a flexible fallible two-way method (success).
OPEN_SERVER_TEST(FlexibleTwoWayErrorSyntaxResponseSuccessResult) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      encode(fidl_serversuite::OpenTargetFlexibleTwoWayErrRequest::WithReplySuccess({})),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({padding(4)}, false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a flexible fallible two-way method (error).
OPEN_SERVER_TEST(FlexibleTwoWayErrorSyntaxResponseErrorResult) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      encode(fidl_serversuite::OpenTargetFlexibleTwoWayErrRequest::WithReplyError(60602293)),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionError),
      inline_envelope({i32(60602293)}, false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a flexible fallible two-way method (nonempty success).
OPEN_SERVER_TEST(FlexibleTwoWayErrorSyntaxNonEmptyResponseSuccessResult) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWayFieldsErr,
             fidl::MessageDynamicFlags::kFlexibleMethod),
      encode(fidl_serversuite::OpenTargetFlexibleTwoWayFieldsErrRequest::WithReplySuccess(406601)),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWayFieldsErr,
             fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({i32(406601)}, false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should reply to a flexible fallible two-way method (nonempty, error).
OPEN_SERVER_TEST(FlexibleTwoWayErrorSyntaxNonEmptyResponseErrorResult) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWayFieldsErr,
             fidl::MessageDynamicFlags::kFlexibleMethod),
      encode(fidl_serversuite::OpenTargetFlexibleTwoWayFieldsErrRequest::WithReplyError(60602293)),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalFlexibleTwoWayFieldsErr,
             fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionError),
      inline_envelope({i32(60602293)}, false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));
}

// The server should tear down when it receives an unknown strict one-way method.
OPEN_SERVER_TEST(UnknownStrictOneWayOpenProtocol) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  ASSERT_FALSE(reporter().received_unknown_method());
}

// The server should run the unknown method handler when it receives an unknown
// flexible one-way method.
OPEN_SERVER_TEST(UnknownFlexibleOneWayOpenProtocol) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

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

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
      handle_present(),
      padding(4),
  };
  HandleDispositions hd_in = {
      zx_handle_disposition_t{
          .handle = event1.release(),
          .type = ZX_OBJ_TYPE_EVENTPAIR,
          .rights = ZX_RIGHT_SAME_RIGHTS,
      },
  };
  ASSERT_OK(client_end().write(bytes_in, hd_in));

  WAIT_UNTIL([this]() { return reporter().received_unknown_method().has_value(); });

  ASSERT_EQ(kOrdinalFakeUnknownMethod, reporter().received_unknown_method()->ordinal());
  ASSERT_EQ(fidl_serversuite::UnknownMethodType::kOneWay,
            reporter().received_unknown_method()->unknown_method_type());

  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));

  ASSERT_OK(event2.wait_one(ZX_EVENTPAIR_PEER_CLOSED, zx::time::infinite_past(), nullptr));
}

// The server should tear down when it receives an unknown strict two-way method.
OPEN_SERVER_TEST(UnknownStrictTwoWayOpenProtocol) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  ASSERT_FALSE(reporter().received_unknown_method());
}

// The server should send an automatic reply and run the unknown method handler
// when it receives an unknown flexible two-way method.
OPEN_SERVER_TEST(UnknownFlexibleTwoWayOpenProtocol) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope(
          {
              transport_err_unknown_method(),
          },
          false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));

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

  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
      handle_present(),
      padding(4),
  };
  HandleDispositions hd_in = {
      zx_handle_disposition_t{
          .handle = event1.release(),
          .type = ZX_OBJ_TYPE_EVENTPAIR,
          .rights = ZX_RIGHT_SAME_RIGHTS,
      },
  };
  ASSERT_OK(client_end().write(bytes_in, hd_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTwoWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope(
          {
              transport_err_unknown_method(),
          },
          false),
  };
  ASSERT_OK(client_end().read_and_check(bytes_out));

  WAIT_UNTIL([this]() { return reporter().received_unknown_method().has_value(); });

  ASSERT_EQ(kOrdinalFakeUnknownMethod, reporter().received_unknown_method()->ordinal());
  ASSERT_EQ(fidl_serversuite::UnknownMethodType::kTwoWay,
            reporter().received_unknown_method()->unknown_method_type());

  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));

  ASSERT_OK(event2.wait_one(ZX_EVENTPAIR_PEER_CLOSED, zx::time::infinite_past(), nullptr));
}

// The ajar server should tear down when it receives an unknown strict one-way method.
AJAR_SERVER_TEST(UnknownStrictOneWayAjarProtocol) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  ASSERT_FALSE(reporter().received_unknown_method());
}

// The ajar server should run the unknown method handler when it receives an unknown
// flexible one-way method.
AJAR_SERVER_TEST(UnknownFlexibleOneWayAjarProtocol) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  WAIT_UNTIL([this]() { return reporter().received_unknown_method().has_value(); });

  ASSERT_EQ(kOrdinalFakeUnknownMethod, reporter().received_unknown_method()->ordinal());
  ASSERT_EQ(fidl_serversuite::UnknownMethodType::kOneWay,
            reporter().received_unknown_method()->unknown_method_type());

  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The ajar server should tear down when it receives an unknown strict two-way method.
AJAR_SERVER_TEST(UnknownStrictTwoWayAjarProtocol) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  ASSERT_FALSE(reporter().received_unknown_method());
}

// The ajar server should tear down when it receives an unknown flexible two-way method.
AJAR_SERVER_TEST(UnknownFlexibleTwoWayAjarProtocol) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
  ASSERT_FALSE(reporter().received_unknown_method());
}

// The closed server should tear down when it receives an unknown strict one-way method.
CLOSED_SERVER_TEST(UnknownStrictOneWayClosedProtocol) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

// The closed server should tear down when it receives an unknown flexible one-way method.
CLOSED_SERVER_TEST(UnknownFlexibleOneWayClosedProtocol) {
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

// The closed server should tear down when it receives an unknown strict two-way method.
CLOSED_SERVER_TEST(UnknownStrictTwoWayClosedProtocol) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

// The closed server should tear down when it receives an unknown flexible two-way method.
CLOSED_SERVER_TEST(UnknownFlexibleTwoWayClosedProtocol) {
  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(client_end().write(bytes_in));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

}  // namespace
}  // namespace server_suite
