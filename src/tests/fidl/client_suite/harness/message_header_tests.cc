// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fidl/fidl.clientsuite/cpp/common_types.h"
#include "fidl/fidl.clientsuite/cpp/natural_types.h"
#include "src/tests/fidl/channel_util/bytes.h"
#include "src/tests/fidl/channel_util/channel.h"
#include "src/tests/fidl/client_suite/harness/harness.h"
#include "src/tests/fidl/client_suite/harness/ordinals.h"

using namespace channel_util;

namespace client_suite {

CLIENT_TEST(ReceiveEventBadMagicNumber) {
  auto reporter = ReceiveClosedEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(0, kOrdinalOnEventNoPayload, fidl::MessageDynamicFlags::kStrictMethod,
             kBadMagicNumber),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });
  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.fidl_error().has_value());
  ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kDecodingError, event.fidl_error().value());

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when an error occurs, but many of them don't actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

CLIENT_TEST(ReceiveEventUnexpectedTxid) {
  auto reporter = ReceiveClosedEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(123 /* should be 0 */, kOrdinalOnEventNoPayload,
             fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });
  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.fidl_error().has_value());
  ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnexpectedMessage, event.fidl_error().value());

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when an error occurs, but many of them don't actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

CLIENT_TEST(ReceiveEventUnknownOrdinal) {
  auto reporter = ReceiveClosedEvents();
  ASSERT_NE(nullptr, reporter);

  Bytes bytes_in = {
      header(0, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL([reporter]() { return reporter->NumReceivedEvents(); });
  ASSERT_EQ(1u, reporter->NumReceivedEvents());
  auto event = reporter->TakeNextEvent();
  ASSERT_TRUE(event.fidl_error().has_value());
  ASSERT_EQ(fidl_clientsuite::FidlErrorKind::kUnexpectedMessage, event.fidl_error().value());

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when an error occurs, but many of them don't actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

CLIENT_TEST(ReceiveResponseBadMagicNumber) {
  runner()->CallTwoWayNoPayload({{.target = TakeClosedClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    EXPECT_EQ(fidl_clientsuite::FidlErrorKind::kDecodingError, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod,
             kBadMagicNumber),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when an error occurs, but many of them don't actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

CLIENT_TEST(ReceiveResponseUnexpectedTxid) {
  if (WaitFor(runner()->GetVersion()).value().version() < 1) {
    GTEST_SKIP() << "Skipping because Runner has not implemented GetBindingsProperties() yet";
  }
  if (WaitFor(runner()->GetBindingsProperties()).value().io_style() ==
      fidl_clientsuite::IoStyle::kSync) {
    GTEST_SKIP() << "Skipping because sync bindings use zx_channel_call, so the thread would "
                    "remain blocked if we respond with a different txid";
  }

  runner()->CallTwoWayNoPayload({{.target = TakeClosedClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    EXPECT_EQ(fidl_clientsuite::FidlErrorKind::kUnexpectedMessage,
              result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  zx_txid_t wrong_txid = 123;
  ASSERT_NE(wrong_txid, txid);

  Bytes bytes_in = {
      header(wrong_txid, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when an error occurs, but many of them don't actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

CLIENT_TEST(ReceiveResponseWrongOrdinalKnown) {
  runner()->CallTwoWayNoPayload({{.target = TakeClosedClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    EXPECT_EQ(fidl_clientsuite::FidlErrorKind::kDecodingError, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalTwoWayStructPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when an error occurs, but many of them don't actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

CLIENT_TEST(ReceiveResponseWrongOrdinalUnknown) {
  runner()->CallTwoWayNoPayload({{.target = TakeClosedClient()}}).ThenExactlyOnce([&](auto result) {
    MarkCallbackRun();
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result.value().fidl_error().has_value());
    EXPECT_EQ(fidl_clientsuite::FidlErrorKind::kDecodingError, result.value().fidl_error().value());
  });

  ASSERT_OK(server_end().wait_for_signal(ZX_CHANNEL_READABLE));

  Bytes bytes_out = {
      header(kTxidNotKnown, kOrdinalTwoWayNoPayload, fidl::MessageDynamicFlags::kStrictMethod),
  };
  zx_txid_t txid;
  ASSERT_OK(server_end().read_and_check_unknown_txid(&txid, bytes_out));
  ASSERT_NE(0u, txid);

  Bytes bytes_in = {
      header(txid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kStrictMethod),
  };
  ASSERT_OK(server_end().write(bytes_in));

  WAIT_UNTIL_CALLBACK_RUN();

  // TODO(fxbug.dev/78906, fxbug.dev/74241): Clients should close the channel
  // when an error occurs, but many of them don't actually.
  // ASSERT_TRUE(server_end().is_signal_present(ZX_CHANNEL_PEER_CLOSED));
}

}  // namespace client_suite
