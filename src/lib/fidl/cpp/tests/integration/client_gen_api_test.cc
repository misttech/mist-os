// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.cpp.constraint.protocol.test/cpp/fidl.h>
#include <fidl/test.basic.protocol/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fit/defer.h>
#include <lib/sync/cpp/completion.h>

#include <zxtest/zxtest.h>

namespace {

using ::test_basic_protocol::Values;

// An integration-style test that verifies that user-supplied async callbacks
// attached using |Then| with client lifetime are not invoked when the client is
// destroyed by the user (i.e. explicit cancellation) instead of due to errors.
template <typename ClientType>
void ThenWithClientLifetimeTest() {
  auto endpoints = fidl::CreateEndpoints<Values>();
  ASSERT_OK(endpoints.status_value());
  auto [local, remote] = std::move(*endpoints);
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  ClientType client(std::move(local), loop.dispatcher());
  bool destroyed = false;

  client->Echo({"foo"}).Then(
      [observer = fit::defer([&] { destroyed = true; })](fidl::Result<Values::Echo>& result) {
        ADD_FATAL_FAILURE("Should not be invoked");
      });
  // Immediately start cancellation.
  client = {};
  ASSERT_FALSE(destroyed);
  loop.RunUntilIdle();

  // The callback should be destroyed without being called.
  ASSERT_TRUE(destroyed);
}

TEST(Client, ThenWithClientLifetime) { ThenWithClientLifetimeTest<fidl::Client<Values>>(); }

TEST(SharedClient, ThenWithClientLifetime) {
  ThenWithClientLifetimeTest<fidl::SharedClient<Values>>();
}

// An integration-style test that verifies that user-supplied async callbacks
// that takes |fidl::WireUnownedResult| are correctly notified when the binding
// is torn down by the user (i.e. explicit cancellation).
template <typename ClientType>
void ThenExactlyOnceTest() {
  auto endpoints = fidl::CreateEndpoints<Values>();
  ASSERT_OK(endpoints.status_value());
  auto [local, remote] = std::move(*endpoints);
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  ClientType client(std::move(local), loop.dispatcher());
  bool called = false;
  bool destroyed = false;
  auto callback_destruction_observer = fit::defer([&] { destroyed = true; });

  client->Echo({"foo"}).ThenExactlyOnce([observer = std::move(callback_destruction_observer),
                                         &called](fidl::Result<Values::Echo>& result) {
    called = true;
    EXPECT_FALSE(result.is_ok());
    EXPECT_STATUS(ZX_ERR_CANCELED, result.error_value().status());
    EXPECT_EQ(fidl::Reason::kUnbind, result.error_value().reason());
  });
  // Immediately start cancellation.
  client = {};
  loop.RunUntilIdle();

  ASSERT_TRUE(called);
  // The callback should be destroyed after being called.
  ASSERT_TRUE(destroyed);
}

TEST(Client, ThenExactlyOnce) { ThenExactlyOnceTest<fidl::Client<Values>>(); }

TEST(SharedClient, ThenExactlyOnce) { ThenExactlyOnceTest<fidl::SharedClient<Values>>(); }

TEST(Client, PropagateEncodeError) {
  using ::fidl_cpp_constraint_protocol_test::Bits;
  using ::fidl_cpp_constraint_protocol_test::Constraint;
  auto endpoints = fidl::CreateEndpoints<Constraint>();
  ASSERT_OK(endpoints.status_value());
  auto [local, remote] = std::move(*endpoints);
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  fidl::Client client(std::move(local), loop.dispatcher());

  static_assert(Bits::TryFrom(0xff) == std::nullopt, "0xff should have invalid bits");
  client->Echo({{.bits = Bits(0xff)}}).ThenExactlyOnce([&](fidl::Result<Constraint::Echo>& result) {
    // 0xff leads to strict bits constraint validation error.
    ASSERT_TRUE(result.is_error());
    EXPECT_EQ(result.error_value().reason(), fidl::Reason::kEncodeError);
  });

  client->Echo({{.bits = Bits::kA}}).ThenExactlyOnce([&](fidl::Result<Constraint::Echo>& result) {
    // |Bits::kA| is valid, but this call is canceled due to the previous error.
    ASSERT_TRUE(result.is_error());
    EXPECT_EQ(result.error_value().reason(), fidl::Reason::kCanceledDueToOtherError);
    EXPECT_EQ(result.error_value().underlying_reason().value(), fidl::Reason::kEncodeError);
    loop.Quit();
  });

  loop.Run();
}

TEST(Client, TwoWayRememberErrorAfterUnbinding) {
  using ::fidl_cpp_constraint_protocol_test::Bits;
  using ::fidl_cpp_constraint_protocol_test::Constraint;
  auto endpoints = fidl::CreateEndpoints<Constraint>();
  ASSERT_OK(endpoints.status_value());
  auto [local, remote] = std::move(*endpoints);
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  struct EventHandler : public fidl::AsyncEventHandler<Constraint> {
    explicit EventHandler(async::Loop& loop) : loop(loop) {}

    void on_fidl_error(::fidl::UnbindInfo error) final {
      EXPECT_EQ(error.reason(), fidl::Reason::kEncodeError);
      loop.Quit();
    }

    async::Loop& loop;
  };
  EventHandler event_handler{loop};

  fidl::Client client(std::move(local), loop.dispatcher(), &event_handler);

  static_assert(Bits::TryFrom(0xff) == std::nullopt, "0xff should have invalid bits");
  client->Echo({{.bits = Bits(0xff)}}).ThenExactlyOnce([&](fidl::Result<Constraint::Echo>& result) {
    // 0xff leads to strict bits constraint validation error.
    ASSERT_TRUE(result.is_error());
    EXPECT_EQ(result.error_value().reason(), fidl::Reason::kEncodeError);
  });

  loop.Run();
  loop.ResetQuit();

  client->Echo({{.bits = Bits::kA}}).ThenExactlyOnce([&](fidl::Result<Constraint::Echo>& result) {
    // |Bits::kA| is valid, but this call is canceled due to the previous error.
    ASSERT_TRUE(result.is_error());
    EXPECT_EQ(result.error_value().reason(), fidl::Reason::kCanceledDueToOtherError);
    EXPECT_EQ(result.error_value().underlying_reason().value(), fidl::Reason::kEncodeError);
    loop.Quit();
  });

  loop.Run();
}

TEST(Client, OneWayRememberErrorAfterUnbinding) {
  auto endpoints = fidl::CreateEndpoints<Values>();
  ASSERT_OK(endpoints.status_value());
  auto [local, remote] = std::move(*endpoints);
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  struct EventHandler : public fidl::AsyncEventHandler<Values> {
    explicit EventHandler(async::Loop& loop) : loop(loop) {}

    void on_fidl_error(::fidl::UnbindInfo error) final {
      EXPECT_EQ(error.reason(), fidl::Reason::kUnexpectedMessage);
      loop.Quit();
    }

    async::Loop& loop;
  };
  EventHandler event_handler{loop};
  fidl::Client client(std::move(local), loop.dispatcher(), &event_handler);

  // Send a malformed message from the server endpoint.
  uint8_t byte = 0x0;
  ASSERT_OK(remote.channel().write(0, &byte, sizeof(byte), nullptr, 0));
  loop.Run();
  loop.ResetQuit();

  fit::result<fidl::OneWayError> result = client->OneWay({"a"});
  // The string is valid, but this call is canceled due to the previous error.
  ASSERT_TRUE(result.is_error());
  EXPECT_EQ(result.error_value().reason(), fidl::Reason::kCanceledDueToOtherError);
  EXPECT_EQ(result.error_value().underlying_reason().value(), fidl::Reason::kUnexpectedMessage);
}

}  // namespace
