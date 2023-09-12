// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.clientsuite/cpp/common_types.h>
#include <fidl/fidl.clientsuite/cpp/natural_types.h>

#include "src/tests/fidl/channel_util/bytes.h"
#include "src/tests/fidl/channel_util/channel.h"
#include "src/tests/fidl/client_suite/harness/harness.h"
#include "src/tests/fidl/client_suite/harness/ordinals.h"

namespace client_suite {
namespace {

using namespace ::channel_util;

// Check that the test runner is set up correctly without doing anything else.
CLIENT_TEST(Setup) {}

// The client should call a two-way method and receive the empty response.
CLIENT_TEST(TwoWayNoPayload) {
  runner()->CallTwoWayNoPayload({{.target = TakeClosedClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().success().has_value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a two-way method and receive the struct response.
CLIENT_TEST(TwoWayStructPayload) {
  static const fidl_clientsuite::NonEmptyPayload kPayload{{.some_field = 42}};

  runner()
      ->CallTwoWayStructPayload({{.target = TakeClosedClient()}})
      .ThenExactlyOnce(
          [&](fidl::Result<fidl_clientsuite::Runner::CallTwoWayStructPayload>& result) {
            MarkCallbackRun();
            ASSERT_TRUE(result.is_ok()) << result.error_value();
            ASSERT_TRUE(result.value().success().has_value());
            ASSERT_EQ(result.value().success().value(), kPayload);
          });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalTwoWayStructPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalTwoWayStructPayload, fidl::MessageDynamicFlags::kStrictMethod),
      i32(42),
      padding(4),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a two-way method and receive the table response.
CLIENT_TEST(TwoWayTablePayload) {
  static const fidl_clientsuite::TablePayload kPayload{{.some_field = 42}};

  runner()
      ->CallTwoWayTablePayload({{.target = TakeClosedClient()}})
      .ThenExactlyOnce([&](fidl::Result<fidl_clientsuite::Runner::CallTwoWayTablePayload>& result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result.value().success().has_value());
        ASSERT_EQ(result.value().success().value(), kPayload);
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalTwoWayTablePayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalTwoWayTablePayload, fidl::MessageDynamicFlags::kStrictMethod),
      encode(kPayload),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a two-way method and receive the union response.
CLIENT_TEST(TwoWayUnionPayload) {
  static const fidl_clientsuite::UnionPayload kPayload =
      fidl_clientsuite::UnionPayload::WithSomeVariant(320494);

  runner()
      ->CallTwoWayUnionPayload({{.target = TakeClosedClient()}})
      .ThenExactlyOnce([&](fidl::Result<fidl_clientsuite::Runner::CallTwoWayUnionPayload>& result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result.value().success().has_value());
        ASSERT_EQ(result.value().success().value(), kPayload);
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalTwoWayUnionPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalTwoWayUnionPayload, fidl::MessageDynamicFlags::kStrictMethod),
      encode(kPayload),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a fallible two-way method and receive the success response.
CLIENT_TEST(TwoWayResultWithPayload) {
  static const fidl_clientsuite::NonEmptyPayload kPayload{{.some_field = 390023}};

  runner()
      ->CallTwoWayStructPayloadErr({{.target = TakeClosedClient()}})
      .ThenExactlyOnce(
          [&](fidl::Result<fidl_clientsuite::Runner::CallTwoWayStructPayloadErr>& result) {
            MarkCallbackRun();
            ASSERT_TRUE(result.is_ok()) << result.error_value();
            ASSERT_TRUE(result.value().success().has_value());
            ASSERT_EQ(result.value().success().value(), kPayload);
          });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalTwoWayStructPayloadErr,
             fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalTwoWayStructPayloadErr, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(kResultUnionSuccess),
      inline_envelope({i32(kPayload.some_field())}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a fallible two-way method and receive the error response.
CLIENT_TEST(TwoWayResultWithError) {
  static const int32_t kError = 90240;

  runner()
      ->CallTwoWayStructPayloadErr({{.target = TakeClosedClient()}})
      .ThenExactlyOnce(
          [&](fidl::Result<fidl_clientsuite::Runner::CallTwoWayStructPayloadErr>& result) {
            MarkCallbackRun();
            ASSERT_TRUE(result.is_ok()) << result.error_value();
            ASSERT_TRUE(result.value().application_error().has_value());
            ASSERT_EQ(result.value().application_error().value(), kError);
          });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalTwoWayStructPayloadErr,
             fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalTwoWayStructPayloadErr, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(kResultUnionError),
      inline_envelope({i32(kError)}, false),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a two-way method with a struct request and receive the response.
CLIENT_TEST(TwoWayStructRequest) {
  static const fidl_clientsuite::NonEmptyPayload kRequest{{.some_field = 390023}};

  runner()
      ->CallTwoWayStructRequest({{.target = TakeClosedClient(), .request = kRequest}})
      .ThenExactlyOnce(
          [&](fidl::Result<fidl_clientsuite::Runner::CallTwoWayStructRequest>& result) {
            MarkCallbackRun();
            ASSERT_TRUE(result.is_ok()) << result.error_value();
            ASSERT_EQ(fidl_clientsuite::EmptyResultClassification::WithSuccess({}), result.value());
          });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalTwoWayStructRequest, fidl::MessageDynamicFlags::kStrictMethod),
      encode(kRequest),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalTwoWayStructRequest, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a two-way method with a table request and receive the response.
CLIENT_TEST(TwoWayTableRequest) {
  static const fidl_clientsuite::TablePayload kRequest{{.some_field = 390023}};

  runner()
      ->CallTwoWayTableRequest({{.target = TakeClosedClient(), .request = kRequest}})
      .ThenExactlyOnce([&](fidl::Result<fidl_clientsuite::Runner::CallTwoWayTableRequest>& result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_EQ(fidl_clientsuite::EmptyResultClassification::WithSuccess({}), result.value());
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(0, kOrdinalTwoWayTableRequest, fidl::MessageDynamicFlags::kStrictMethod),
      encode(kRequest),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalTwoWayTableRequest, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a two-way method with a union request and receive the response.
CLIENT_TEST(TwoWayUnionRequest) {
  static const fidl_clientsuite::UnionPayload kRequest =
      fidl_clientsuite::UnionPayload::WithSomeVariant(390023);

  runner()
      ->CallTwoWayUnionRequest({{.target = TakeClosedClient(), .request = kRequest}})
      .ThenExactlyOnce([&](fidl::Result<fidl_clientsuite::Runner::CallTwoWayUnionRequest>& result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_EQ(fidl_clientsuite::EmptyResultClassification::WithSuccess({}), result.value());
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(0, kOrdinalTwoWayUnionRequest, fidl::MessageDynamicFlags::kStrictMethod),
      encode(kRequest),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalTwoWayUnionRequest, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a one-way method with an empty request.
CLIENT_TEST(OneWayNoRequest) {
  runner()
      ->CallOneWayNoRequest({{.target = TakeClosedClient()}})
      .ThenExactlyOnce([&](fidl::Result<fidl_clientsuite::Runner::CallOneWayNoRequest>& result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_EQ(fidl_clientsuite::EmptyResultClassification::WithSuccess({}), result.value());
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(0, kOrdinalOneWayNoRequest, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().read_and_check(bytes_out));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a one-way method with a struct request.
CLIENT_TEST(OneWayStructRequest) {
  static const fidl_clientsuite::NonEmptyPayload kRequest{{.some_field = 390023}};

  runner()
      ->CallOneWayStructRequest({{.target = TakeClosedClient(), .request = kRequest}})
      .ThenExactlyOnce(
          [&](fidl::Result<fidl_clientsuite::Runner::CallOneWayStructRequest>& result) {
            MarkCallbackRun();
            ASSERT_TRUE(result.is_ok()) << result.error_value();
            ASSERT_EQ(fidl_clientsuite::EmptyResultClassification::WithSuccess({}), result.value());
          });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(0, kOrdinalOneWayStructRequest, fidl::MessageDynamicFlags::kStrictMethod),
      encode(kRequest),
  };
  ASSERT_OK(server_end().read_and_check(bytes_out));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a one-way method with a table request.
CLIENT_TEST(OneWayTableRequest) {
  static const fidl_clientsuite::TablePayload kRequest{{.some_field = 390023}};

  runner()
      ->CallOneWayTableRequest({{.target = TakeClosedClient(), .request = kRequest}})
      .ThenExactlyOnce([&](fidl::Result<fidl_clientsuite::Runner::CallOneWayTableRequest>& result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_EQ(fidl_clientsuite::EmptyResultClassification::WithSuccess({}), result.value());
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(0, kOrdinalOneWayTableRequest, fidl::MessageDynamicFlags::kStrictMethod),
      encode(kRequest),
  };
  ASSERT_OK(server_end().read_and_check(bytes_out));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should call a one-way method with a union request.
CLIENT_TEST(OneWayUnionRequest) {
  static const fidl_clientsuite::UnionPayload kRequest =
      fidl_clientsuite::UnionPayload::WithSomeVariant(390023);

  runner()
      ->CallOneWayUnionRequest({{.target = TakeClosedClient(), .request = kRequest}})
      .ThenExactlyOnce([&](fidl::Result<fidl_clientsuite::Runner::CallOneWayUnionRequest>& result) {
        MarkCallbackRun();
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_EQ(fidl_clientsuite::EmptyResultClassification::WithSuccess({}), result.value());
      });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(0, kOrdinalOneWayUnionRequest, fidl::MessageDynamicFlags::kStrictMethod),
      encode(kRequest),
  };
  ASSERT_OK(server_end().read_and_check(bytes_out));

  WAIT_UNTIL_CALLBACK_RUN();
}

// The client should receive an event with no payload.
CLIENT_TEST(ReceiveEventNoPayload) {
  auto reporter = ReceiveClosedEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalOnEventNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.on_event_no_payload().has_value());
}

// The client should receive an event with a struct.
CLIENT_TEST(ReceiveEventStructPayload) {
  static const fidl_clientsuite::NonEmptyPayload kRequest{{.some_field = 9098607}};

  auto reporter = ReceiveClosedEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalOnEventStructPayload, fidl::MessageDynamicFlags::kStrictMethod),
      encode(kRequest),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.on_event_struct_payload().has_value());
  EXPECT_EQ(kRequest, event.on_event_struct_payload().value());
}

// The client should receive an event with a table.
CLIENT_TEST(ReceiveEventTablePayload) {
  static const fidl_clientsuite::TablePayload kRequest{{.some_field = 9098607}};

  auto reporter = ReceiveClosedEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalOnEventTablePayload, fidl::MessageDynamicFlags::kStrictMethod),
      encode(kRequest),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.on_event_table_payload().has_value());
  EXPECT_EQ(kRequest, event.on_event_table_payload().value());
}

// The client should receive an event with a union.
CLIENT_TEST(ReceiveEventUnionPayload) {
  static const fidl_clientsuite::UnionPayload kRequest =
      fidl_clientsuite::UnionPayload::WithSomeVariant(87662);

  auto reporter = ReceiveClosedEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalOnEventUnionPayload, fidl::MessageDynamicFlags::kStrictMethod),
      encode(kRequest),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });

  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.on_event_union_payload().has_value());
  EXPECT_EQ(kRequest, event.on_event_union_payload().value());
}

// The client should fail to call a two-way method after the server closes the channel.
CLIENT_TEST(GracefulFailureDuringCallAfterPeerClose) {
  server_end().get().reset();

  runner()->CallTwoWayNoPayload({{.target = TakeClosedClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kChannelPeerClosed,
              result.value().fidl_error().value());
  });

  WAIT_UNTIL_CALLBACK_RUN();
}

}  // namespace
}  // namespace client_suite
