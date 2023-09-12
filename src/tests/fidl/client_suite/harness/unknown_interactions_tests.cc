// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.clientsuite/cpp/common_types.h>
#include <fidl/fidl.clientsuite/cpp/natural_types.h>

#include <memory>

#include "src/tests/fidl/client_suite/harness/harness.h"
#include "src/tests/fidl/client_suite/harness/ordinals.h"

namespace client_suite {
namespace {

using namespace ::channel_util;

// The client should call a strict one-way method.
CLIENT_TEST(OneWayStrictSend) {
  runner()->CallStrictOneWay({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().success().has_value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kOneWayTxid, kOrdinalStrictOneWay, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().read_and_check(bytes_out));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a flexible one-way method.
CLIENT_TEST(OneWayFlexibleSend) {
  runner()->CallFlexibleOneWay({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().success().has_value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kOneWayTxid, kOrdinalFlexibleOneWay, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(server_end().read_and_check(bytes_out));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a strict two-way method and receive the response.
CLIENT_TEST(TwoWayStrictSend) {
  runner()->CallStrictTwoWay({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().success().has_value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalStrictTwoWay, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalStrictTwoWay, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a strict two-way method and receive the response,
// despite the schema (strict) not matching the response's dynamic flags (flexible).
CLIENT_TEST(TwoWayStrictSendMismatchedStrictness) {
  runner()->CallStrictTwoWay({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().success().has_value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalStrictTwoWay, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalStrictTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a strict two-way method and receive the nonempty response.
CLIENT_TEST(TwoWayStrictSendNonEmptyPayload) {
  runner()
      ->CallStrictTwoWayFields({{.target = TakeOpenClient()}})
      .ThenExactlyOnce([&](auto result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result.value().success().has_value());
        ASSERT_EQ(fidl_clientsuite::NonEmptyPayload(541768), result.value().success().value());
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalStrictTwoWayFields, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalStrictTwoWayFields, fidl::MessageDynamicFlags::kStrictMethod),
      encode(fidl_clientsuite::NonEmptyPayload(541768)),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a strict fallible two-way method and receive the success response.
CLIENT_TEST(TwoWayStrictErrorSyntaxSendSuccessResponse) {
  runner()->CallStrictTwoWayErr({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().success().has_value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({padding(4)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a strict fallible two-way method and receive the error response.
CLIENT_TEST(TwoWayStrictErrorSyntaxSendErrorResponse) {
  static constexpr int32_t kApplicationError = 39243320;

  runner()->CallStrictTwoWayErr({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().application_error().has_value());
    ASSERT_EQ(kApplicationError, result.value().application_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(kResultUnionError),
      inline_envelope({i32(kApplicationError)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should tear down when it calls a strict fallible two-way method and
// receives an "unknown method" response (with strict dynamic flag).
CLIENT_TEST(TwoWayStrictErrorSyntaxSendUnknownMethodResponse) {
  runner()->CallStrictTwoWayErr({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kDecodingError, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope({i32(ZX_ERR_NOT_SUPPORTED)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should tear down when it calls a strict fallible two-way method
// and receives an "unknown method" response (with flexible dynamic flag).
CLIENT_TEST(TwoWayStrictErrorSyntaxSendMismatchedStrictnessUnknownMethodResponse) {
  runner()->CallStrictTwoWayErr({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kDecodingError, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalStrictTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope({i32(ZX_ERR_NOT_SUPPORTED)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a strict fallible two-way method and receive the
// nonempty success response.
CLIENT_TEST(TwoWayStrictErrorSyntaxSendNonEmptyPayload) {
  runner()
      ->CallStrictTwoWayFieldsErr({{.target = TakeOpenClient()}})
      .ThenExactlyOnce([&](auto result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result.value().success().has_value());
        ASSERT_EQ(fidl_clientsuite::NonEmptyPayload(394966), result.value().success().value());
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalStrictTwoWayFieldsErr,
             fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalStrictTwoWayFieldsErr, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({i32(394966)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a flexible two-way method and receive the empty response.
CLIENT_TEST(TwoWayFlexibleSendSuccessResponse) {
  runner()->CallFlexibleTwoWay({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().success().has_value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({padding(4)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should tear down when it calls a flexible two-way method and receives
// a domain error response, which is invalid for a method without error syntax.
CLIENT_TEST(TwoWayFlexibleSendErrorResponse) {
  runner()->CallFlexibleTwoWay({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kDecodingError, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionError),
      inline_envelope({i32(39205950)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a flexible two-way method and accept the "unknown method" response.
CLIENT_TEST(TwoWayFlexibleSendUnknownMethodResponse) {
  runner()->CallFlexibleTwoWay({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnknownMethod, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope({i32(ZX_ERR_NOT_SUPPORTED)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// TODO(fxbug.dev/133435): This test is incorrect. The client should tear down
// because the response message is inconsistent. Once fixed, comment should be:
// > The client should tear down when it calls a flexible two-way method and
// > receives an "unknown method" response (with strict dynamic flag).
CLIENT_TEST(TwoWayFlexibleSendMismatchedStrictnessUnknownMethodResponse) {
  runner()->CallFlexibleTwoWay({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnknownMethod, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope({i32(ZX_ERR_NOT_SUPPORTED)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should tear down when it calls a flexible two-way method and
// receives a framework error response other than "unsupported method".
CLIENT_TEST(TwoWayFlexibleSendOtherTransportErrResponse) {
  runner()->CallFlexibleTwoWay({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kDecodingError, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWay, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope({i32(ZX_ERR_ACCESS_DENIED)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a flexible two-way method and receive the nonempty response.
CLIENT_TEST(TwoWayFlexibleSendNonEmptyPayloadSuccessResponse) {
  static constexpr int32_t kSomeFieldValue = 302340665;

  runner()
      ->CallFlexibleTwoWayFields({{.target = TakeOpenClient()}})
      .ThenExactlyOnce([&](auto result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result.value().success().has_value());
        ASSERT_EQ(fidl_clientsuite::NonEmptyPayload(kSomeFieldValue),
                  result.value().success().value());
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWayFields,
             fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWayFields, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({i32(kSomeFieldValue)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a flexible two-way method whose response is nonempty,
// and accept the "unknown method" response.
CLIENT_TEST(TwoWayFlexibleSendNonEmptyPayloadUnknownMethodResponse) {
  runner()
      ->CallFlexibleTwoWayFields({{.target = TakeOpenClient()}})
      .ThenExactlyOnce([&](auto result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result.value().fidl_error().has_value());
        ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnknownMethod,
                  result.value().fidl_error().value());
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWayFields,
             fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWayFields, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope({i32(ZX_ERR_NOT_SUPPORTED)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a flexible fallible two-way method and receive the success response.
CLIENT_TEST(TwoWayFlexibleErrorSyntaxSendSuccessResponse) {
  runner()->CallFlexibleTwoWayErr({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().success().has_value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({padding(4)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a flexible fallible two-way method and receive the error response.
CLIENT_TEST(TwoWayFlexibleErrorSyntaxSendErrorResponse) {
  static constexpr int32_t kApplicationError = 1456681;

  runner()->CallFlexibleTwoWayErr({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().application_error().has_value());
    ASSERT_EQ(kApplicationError, result.value().application_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionError),
      inline_envelope({i32(kApplicationError)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a flexible fallible two-way method accept the "unknown method" response.
CLIENT_TEST(TwoWayFlexibleErrorSyntaxSendUnknownMethodResponse) {
  runner()->CallFlexibleTwoWayErr({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnknownMethod, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope({i32(ZX_ERR_NOT_SUPPORTED)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// TODO(fxbug.dev/133435): This test is incorrect. The client should tear down
// because the response message is inconsistent. Once fixed, comment should be:
// > The client should tear down when it calls a flexible fallible two-way method
// > and receives an "unknown method" response (with strict dynamic flag).
CLIENT_TEST(TwoWayFlexibleErrorSyntaxSendMismatchedStrictnessUnknownMethodResponse) {
  runner()->CallFlexibleTwoWayErr({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnknownMethod, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope({i32(ZX_ERR_NOT_SUPPORTED)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should tear down when it calls a flexible fallible two-way method
// and receives a framework error response other than "unsupported method".
CLIENT_TEST(TwoWayFlexibleErrorSyntaxSendOtherTransportErrResponse) {
  runner()->CallFlexibleTwoWayErr({{.target = TakeOpenClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kDecodingError, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWayErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope({i32(ZX_ERR_ACCESS_DENIED)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a flexible fallible two-way method and receive
// the nonempty success response.
CLIENT_TEST(TwoWayFlexibleErrorSyntaxSendNonEmptyPayloadSuccessResponse) {
  static constexpr int32_t kSomeFieldValue = 670705054;

  runner()
      ->CallFlexibleTwoWayFieldsErr({{.target = TakeOpenClient()}})
      .ThenExactlyOnce([&](auto result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result.value().success().has_value());
        ASSERT_EQ(fidl_clientsuite::NonEmptyPayload(kSomeFieldValue),
                  result.value().success().value());
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWayFieldsErr,
             fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWayFieldsErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({i32(kSomeFieldValue)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a flexible fallible two-way method whose response is
// nonempty, and accept the "unknown method" response.
CLIENT_TEST(TwoWayFlexibleErrorSyntaxSendNonEmptyPayloadUnknownMethodResponse) {
  runner()
      ->CallFlexibleTwoWayFieldsErr({{.target = TakeOpenClient()}})
      .ThenExactlyOnce([&](auto result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result.value().fidl_error().has_value());
        ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnknownMethod,
                  result.value().fidl_error().value());
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalFlexibleTwoWayFieldsErr,
             fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFlexibleTwoWayFieldsErr, fidl::MessageDynamicFlags::kFlexibleMethod),
      union_ordinal(kResultUnionTransportError),
      inline_envelope({i32(ZX_ERR_NOT_SUPPORTED)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should receive a strict event.
CLIENT_TEST(ReceiveStrictEvent) {
  auto reporter = ReceiveOpenEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalStrictEvent, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.strict_event().has_value());

  ASSERT_FALSE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The client should receive an event, despite the schema (strict) not matching
// the dynamic flags (flexible).
CLIENT_TEST(ReceiveStrictEventMismatchedStrictness) {
  auto reporter = ReceiveOpenEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalStrictEvent, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.strict_event().has_value());

  ASSERT_FALSE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The client should receive a flexible event.
CLIENT_TEST(ReceiveFlexibleEvent) {
  auto reporter = ReceiveOpenEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFlexibleEvent, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.flexible_event().has_value());

  ASSERT_FALSE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The client should receive an event, despite the schema (flexible) not
// matching the dynamic flags (strict).
CLIENT_TEST(ReceiveFlexibleEventMismatchedStrictness) {
  auto reporter = ReceiveOpenEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFlexibleEvent, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.flexible_event().has_value());

  ASSERT_FALSE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The open client should tear down when it receives an unknown strict event.
CLIENT_TEST(UnknownStrictEventOpenProtocol) {
  auto reporter = ReceiveOpenEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.fidl_error().has_value());
  ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnexpectedMessage, event.fidl_error().value());

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when they receive an unsupported unknown event, but many of them don't
  // actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The open client should accept an unknown flexible event.
CLIENT_TEST(UnknownFlexibleEventOpenProtocol) {
  auto reporter = ReceiveOpenEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.unknown_event().has_value());
  ASSERT_EQ(fidl_clientsuite::UnknownEvent(kOrdinalFakeUnknownMethod),
            event.unknown_event().value());

  ASSERT_FALSE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The ajar client should tear down when it receives an unknown strict event.
CLIENT_TEST(UnknownStrictEventAjarProtocol) {
  auto reporter = ReceiveAjarEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.fidl_error().has_value());
  ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnexpectedMessage, event.fidl_error().value());

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when they receive an unsupported unknown event, but many of them don't
  // actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The ajar client should accept an unknown flexible event.
CLIENT_TEST(UnknownFlexibleEventAjarProtocol) {
  auto reporter = ReceiveAjarEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.unknown_event().has_value());
  ASSERT_EQ(fidl_clientsuite::UnknownEvent(kOrdinalFakeUnknownMethod),
            event.unknown_event().value());

  ASSERT_FALSE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The closed client should tear down when it receives an unknown strict event.
CLIENT_TEST(UnknownStrictEventClosedProtocol) {
  auto reporter = ReceiveClosedEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.fidl_error().has_value());
  ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnexpectedMessage, event.fidl_error().value());

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when they receive an unsupported unknown event, but many of them don't
  // actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The closed client should tear down when it receives an unknown flexible event.
CLIENT_TEST(UnknownFlexibleEventClosedProtocol) {
  auto reporter = ReceiveClosedEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.fidl_error().has_value());
  ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnexpectedMessage, event.fidl_error().value());

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when they receive an unsupported unknown event, but many of them don't
  // actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The client should tear down when it receives an unsolicited strict message
// with nonzero txid and an unknown ordinal.
CLIENT_TEST(UnknownStrictServerInitiatedTwoWay) {
  auto reporter = ReceiveOpenEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.fidl_error().has_value());
  ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnexpectedMessage, event.fidl_error().value());

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when they receive an unsupported unknown event, but many of them don't
  // actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

// The client should tear down when it receives an unsolicited flexible message
// with nonzero txid and an unknown ordinal.
CLIENT_TEST(UnknownFlexibleServerInitiatedTwoWay) {
  auto reporter = ReceiveOpenEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kTwoWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.fidl_error().has_value());
  ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnexpectedMessage, event.fidl_error().value());

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when they receive an unsupported unknown event, but many of them don't
  // actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

}  // namespace
}  // namespace client_suite
