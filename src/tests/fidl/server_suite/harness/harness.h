// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_TESTS_FIDL_SERVER_SUITE_HARNESS_HARNESS_H_
#define SRC_TESTS_FIDL_SERVER_SUITE_HARNESS_HARNESS_H_

#include <fidl/fidl.serversuite/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <optional>

#include <gtest/gtest.h>

#include "fidl/fidl.serversuite/cpp/markers.h"
#include "fidl/fidl.serversuite/cpp/natural_types.h"
#include "lib/fidl/cpp/client.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/lib/testing/predicates/status.h"
#include "src/tests/fidl/channel_util/channel.h"

#define WAIT_UNTIL(condition) ASSERT_TRUE(_wait_until(condition));

#define WAIT_UNTIL_EXT(ext, condition) ASSERT_TRUE((ext)->_wait_until(condition));

#define WAIT_UNTIL_CONTROLLER_CALLBACK_RUN() \
  ASSERT_TRUE(_wait_until([&]() { return WasControllerCallbackRun(); }));

#define EXPECT_TEARDOWN_REASON(reason)     \
  do {                                     \
    SCOPED_TRACE("Check teardown reason"); \
    _check_teardown_reason(reason);        \
  } while (false);

// Defines a new server test. Relies on gtest under the hood.
// Tests must use upper camel case names and be defined in the |Test| enum in
// serversuite.test.fidl.
#define SERVER_TEST(test_name, target_type)                                              \
  struct ServerTestWrapper##test_name : public ServerTest<target_type> {                 \
    ServerTestWrapper##test_name() : ServerTest(fidl_serversuite::Test::k##test_name) {} \
  };                                                                                     \
  TEST_F(ServerTestWrapper##test_name, test_name)

#define CLOSED_SERVER_TEST(test_name) \
  SERVER_TEST(test_name, fidl_serversuite::AnyTarget::Tag::kClosedTarget)
#define AJAR_SERVER_TEST(test_name) \
  SERVER_TEST(test_name, fidl_serversuite::AnyTarget::Tag::kAjarTarget)
#define OPEN_SERVER_TEST(test_name) \
  SERVER_TEST(test_name, fidl_serversuite::AnyTarget::Tag::kOpenTarget)

namespace server_suite {

template <typename Protocol>
class TeardownReasonReporterMixin : public virtual fidl::AsyncEventHandler<Protocol> {
  // Composing protocols isn't inheritance, so to implement the composed
  // "BaseTargetController" protocol this way, we have to inherit from the
  // AsyncEventHandler for the specific protocol.

 public:
  TeardownReasonReporterMixin() = default;
  ~TeardownReasonReporterMixin() override = default;

  void WillTeardown(fidl::Event<typename Protocol::WillTeardown>& event) override {
    if (teardown_reason_.has_value()) {
      ADD_FAILURE() << "The server under test should not report more than one teardown reason.";
      return;
    }
    teardown_reason_.emplace(event.reason());
  }

  std::optional<fidl_serversuite::TeardownReason> teardown_reason() const {
    return teardown_reason_;
  }

 private:
  std::optional<fidl_serversuite::TeardownReason> teardown_reason_;
};

template <typename Protocol>
class UnknownInteractionsReporterMixin : public virtual fidl::AsyncEventHandler<Protocol> {
  // Composing protocols isn't inheritance, so to implement the composed
  // "UnknownInteractionControllerMixin" protocol this way, we have to inherit
  // from the AsyncEventHandler for the specific protocol.

 public:
  UnknownInteractionsReporterMixin() = default;
  ~UnknownInteractionsReporterMixin() override = default;

  void ReceivedUnknownMethod(
      fidl::Event<typename Protocol::ReceivedUnknownMethod>& event) override {
    unknown_method_info_ = std::move(event);
  }

  std::optional<fidl_serversuite::UnknownMethodInfo> received_unknown_method() const {
    return unknown_method_info_;
  }

 private:
  std::optional<fidl_serversuite::UnknownMethodInfo> unknown_method_info_;
};

template <typename Protocol>
class ReplyEncodingReporterMixin : public virtual fidl::AsyncEventHandler<Protocol> {
  // Composing protocols isn't inheritance, so to implement the composed
  // "ReplyEncodingReporterMixin" protocol this way, we have to inherit
  // from the AsyncEventHandler for the specific protocol.

 public:
  ReplyEncodingReporterMixin() = default;
  ~ReplyEncodingReporterMixin() override = default;

  void ReplyEncodingFailed(fidl::Event<typename Protocol::ReplyEncodingFailed>& event) override {
    reply_encoding_failed_.emplace_back(std::move(event));
  }

  // If there were no ReplyEncodingFailed calls, returns |std::nullopt|. If
  // there was exactly one failure, returns that failure. If there is more than
  // one failure, reports an EXPECT_ error and returns the last error received.
  std::optional<fidl_serversuite::EncodingFailureInfo> reply_encoding_failed() const {
    if (reply_encoding_failed_.empty()) {
      return std::nullopt;
    }
    EXPECT_EQ(reply_encoding_failed_.size(), 1u);
    return reply_encoding_failed_.back();
  }

  const std::vector<fidl_serversuite::EncodingFailureInfo>& reply_encoding_failures() const {
    return reply_encoding_failed_;
  }

 private:
  std::vector<fidl_serversuite::EncodingFailureInfo> reply_encoding_failed_;
};

class ClosedEventReporter
    : public virtual fidl::AsyncEventHandler<fidl_serversuite::ClosedTargetController>,
      public TeardownReasonReporterMixin<fidl_serversuite::ClosedTargetController> {
 public:
  void ReceivedOneWayNoPayload() override;
  bool received_one_way_no_payload() const { return received_one_way_no_payload_; }

 private:
  bool received_one_way_no_payload_ = false;
};

class AjarEventReporter
    : public virtual fidl::AsyncEventHandler<fidl_serversuite::AjarTargetController>,
      public TeardownReasonReporterMixin<fidl_serversuite::AjarTargetController>,
      public UnknownInteractionsReporterMixin<fidl_serversuite::AjarTargetController> {};

class OpenEventReporter
    : public virtual fidl::AsyncEventHandler<fidl_serversuite::OpenTargetController>,
      public TeardownReasonReporterMixin<fidl_serversuite::OpenTargetController>,
      public UnknownInteractionsReporterMixin<fidl_serversuite::OpenTargetController> {
 public:
  void ReceivedStrictOneWay() override;
  bool received_strict_one_way() const { return received_strict_one_way_; }

  void ReceivedFlexibleOneWay() override;
  bool received_flexible_one_way() const { return received_flexible_one_way_; }

 private:
  bool received_strict_one_way_ = false;
  bool received_flexible_one_way_ = false;
};

template <fidl_serversuite::AnyTarget::Tag TARGET_TYPE>
struct TargetTypes;

template <>
struct TargetTypes<fidl_serversuite::AnyTarget::Tag::kClosedTarget> {
  using Target = fidl_serversuite::ClosedTarget;
  using Controller = fidl_serversuite::ClosedTargetController;
  using Reporter = ClosedEventReporter;
};

template <>
struct TargetTypes<fidl_serversuite::AnyTarget::Tag::kAjarTarget> {
  using Target = fidl_serversuite::AjarTarget;
  using Controller = fidl_serversuite::AjarTargetController;
  using Reporter = AjarEventReporter;
};

template <>
struct TargetTypes<fidl_serversuite::AnyTarget::Tag::kOpenTarget> {
  using Target = fidl_serversuite::OpenTarget;
  using Controller = fidl_serversuite::OpenTargetController;
  using Reporter = OpenEventReporter;
};

template <fidl_serversuite::AnyTarget::Tag TARGET_TYPE>
using Target = typename TargetTypes<TARGET_TYPE>::Target;

template <fidl_serversuite::AnyTarget::Tag TARGET_TYPE>
using TargetController = typename TargetTypes<TARGET_TYPE>::Controller;

template <fidl_serversuite::AnyTarget::Tag TARGET_TYPE>
using TargetReporter = typename TargetTypes<TARGET_TYPE>::Reporter;

template <fidl_serversuite::AnyTarget::Tag TARGET_TYPE>
class ServerTest : private ::loop_fixture::RealLoop, public ::testing::Test {
 public:
  static constexpr zx::duration kTimeoutDuration = zx::sec(5);

  explicit ServerTest(fidl_serversuite::Test test) : test_(test) {}

  void SetUp() override {
    auto runner_service = component::Connect<fidl_serversuite::Runner>();
    ASSERT_OK(runner_service.status_value());
    runner_ = fidl::SyncClient<fidl_serversuite::Runner>(std::move(*runner_service));

    // Ensure the process hasn't crashed from a previous iteration.
    auto check_alive_result = runner_->CheckAlive();
    ASSERT_TRUE(check_alive_result.is_ok()) << check_alive_result.error_value();

    auto is_test_enabled_result = runner_->IsTestEnabled(test_);
    ASSERT_TRUE(is_test_enabled_result.is_ok()) << is_test_enabled_result.error_value();
    if (!is_test_enabled_result->is_enabled()) {
      GTEST_SKIP() << "(test skipped by binding server)";
      return;
    }

    auto is_teardown_reason_supported = runner_->IsTeardownReasonSupported();
    ASSERT_TRUE(is_teardown_reason_supported.is_ok()) << is_teardown_reason_supported.error_value();
    is_teardown_reason_supported_ = is_teardown_reason_supported.value().is_supported();

    // Create Reporter, which will allow the binding server to report test progress.
    auto controller_endpoints = fidl::CreateEndpoints<TargetController<TARGET_TYPE>>();
    ASSERT_OK(controller_endpoints.status_value()) << controller_endpoints.status_string();

    auto target_endpoints = fidl::CreateEndpoints<Target<TARGET_TYPE>>();
    ASSERT_OK(target_endpoints.status_value()) << target_endpoints.status_string();
    target_ = channel_util::Channel(target_endpoints->client.TakeChannel());

    controller_ = fidl::Client<TargetController<TARGET_TYPE>>(
        std::move(controller_endpoints->client), dispatcher(), &reporter_);

    std::optional<fidl_serversuite::AnyTarget> target_server;
    if constexpr (TARGET_TYPE == fidl_serversuite::AnyTarget::Tag::kClosedTarget) {
      target_server = fidl_serversuite::AnyTarget::WithClosedTarget({{
          .controller = std::move(controller_endpoints->server),
          .sut = std::move(target_endpoints->server),
      }});
    } else if constexpr (TARGET_TYPE == fidl_serversuite::AnyTarget::Tag::kAjarTarget) {
      target_server = fidl_serversuite::AnyTarget::WithAjarTarget({{
          .controller = std::move(controller_endpoints->server),
          .sut = std::move(target_endpoints->server),
      }});
    } else if constexpr (TARGET_TYPE == fidl_serversuite::AnyTarget::Tag::kOpenTarget) {
      target_server = fidl_serversuite::AnyTarget::WithOpenTarget({{
          .controller = std::move(controller_endpoints->server),
          .sut = std::move(target_endpoints->server),
      }});
    } else {
      ZX_PANIC("Unreachable");
    }

    auto start_result =
        runner_->Start(fidl_serversuite::RunnerStartRequest(std::move(target_server.value())));
    ASSERT_TRUE(start_result.is_ok()) << start_result.error_value();

    ASSERT_OK(target_.get().get_info(ZX_INFO_HANDLE_VALID, nullptr, 0, nullptr, nullptr));
  }

  void TearDown() override {
    // Close the Target channel so it will not continue waiting for requests.
    target_.reset();
    controller_ = std::nullopt;
    // Run until idle because client of controller_ requires to run tasks on the
    // dispatcher (otherwise the channel won't close and the corresponding
    // server end will stay running).
    RunLoopUntilIdle();

    // Ensure the process hasn't crashed unexpectedly.
    auto result = runner_->CheckAlive();
    ASSERT_TRUE(result.is_ok()) << "CheckAlive failed! The server_suite runner may have crashed "
                                   "due to a bug in the binding under test. More details: "
                                << result.error_value();
  }

  const TargetReporter<TARGET_TYPE>& reporter() { return reporter_; }
  const fidl::Client<TargetController<TARGET_TYPE>>& controller() { return controller_.value(); }
  channel_util::Channel& client_end() { return target_; }

  // Use WAIT_UNTIL or WAIT_UNTIL_EXT instead of calling |_wait_until| directly.
  bool _wait_until(fit::function<bool()> condition) {
    return RunLoopWithTimeoutOrUntil(std::move(condition), kTimeoutDuration);
  }

  // Use EXPECT_TEARDOWN_REASON instead of calling |_check_teardown_reason| directly.
  void _check_teardown_reason(fidl_serversuite::TeardownReason reason) {
    if (!reporter().teardown_reason().has_value()) {
      ADD_FAILURE() << "Did not get a teardown reason";
      return;
    }
    if (is_teardown_reason_supported()) {
      EXPECT_EQ(reason, reporter().teardown_reason().value());
    } else {
      EXPECT_EQ(fidl_serversuite::TeardownReason::kOther, reporter().teardown_reason().value());
    }
  }

  bool is_teardown_reason_supported() const { return is_teardown_reason_supported_; }

  // Mark that the callback for a controller two-way method was run.
  void MarkControllerCallbackRun() { ran_controller_callback_ = true; }
  // Check whether the callback for a controller two-way method was run.
  bool WasControllerCallbackRun() const { return ran_controller_callback_; }

 private:
  fidl_serversuite::Test test_;

  fidl::SyncClient<fidl_serversuite::Runner> runner_;

  bool is_teardown_reason_supported_ = false;

  channel_util::Channel target_;
  TargetReporter<TARGET_TYPE> reporter_;
  std::optional<fidl::Client<TargetController<TARGET_TYPE>>> controller_;

  bool ran_controller_callback_ = false;
};

// Transaction ID to use for one-way methods. This is the actual value that's
// always used in production.
const zx_txid_t kOneWayTxid = 0;

// Transaction ID to use for two-way methods in tests. This is a randomly chosen representative
// value which is only for this particular test. Tests should avoid using |0x174d3b6a| for other
// sentinel values, to make binary output easier to scan.
const zx_txid_t kTwoWayTxid = 0x174d3b6a;

// This byte is purposely set to a random chosen value that is neither |0x00| nor |0xff|, to help
// easily distinguish its appearance in encoded FIDL relative to present/absent headers, padding,
// and the like.
const uint8_t kSomeByte = 0x42;

}  // namespace server_suite

#endif  // SRC_TESTS_FIDL_SERVER_SUITE_HARNESS_HARNESS_H_
