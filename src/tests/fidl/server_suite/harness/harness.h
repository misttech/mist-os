// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_TESTS_FIDL_SERVER_SUITE_HARNESS_HARNESS_H_
#define SRC_TESTS_FIDL_SERVER_SUITE_HARNESS_HARNESS_H_

#include <fidl/fidl.serversuite/cpp/fidl.h>
#include <lib/component/incoming/cpp/service_client.h>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "src/lib/testing/predicates/status.h"
#include "src/tests/fidl/channel_util/channel.h"

#define WAIT_UNTIL(condition) ASSERT_TRUE(_wait_until(condition));

#define WAIT_UNTIL_EXT(ext, condition) ASSERT_TRUE((ext)->_wait_until(condition));

#define EXPECT_TEARDOWN_REASON(reason)     \
  do {                                     \
    SCOPED_TRACE("Check teardown reason"); \
    _check_teardown_reason(reason);        \
  } while (false);

// Defines a new server test. Relies on gtest under the hood.
// Tests must use upper camel case names and be defined in the |Test| enum in
// serversuite.test.fidl.
#define SERVER_TEST(test_name, target_type)                                \
  struct ServerTestWrapper##test_name : public ServerTest {                \
    ServerTestWrapper##test_name()                                         \
        : ServerTest(fidl_serversuite::Test::k##test_name, target_type) {} \
  };                                                                       \
  TEST_F(ServerTestWrapper##test_name, test_name)

#define CLOSED_SERVER_TEST(test_name) \
  SERVER_TEST(test_name, fidl_serversuite::AnyTarget::Tag::kClosedTarget)
#define AJAR_SERVER_TEST(test_name) \
  SERVER_TEST(test_name, fidl_serversuite::AnyTarget::Tag::kAjarTarget)
#define OPEN_SERVER_TEST(test_name) \
  SERVER_TEST(test_name, fidl_serversuite::AnyTarget::Tag::kOpenTarget)
#define LARGE_MESSAGE_SERVER_TEST(test_name) \
  SERVER_TEST(test_name, fidl_serversuite::AnyTarget::Tag::kLargeMessageTarget)

namespace server_suite {

class Reporter : public fidl::Server<fidl_serversuite::Reporter> {
 public:
  void ReceivedOneWayNoPayload(ReceivedOneWayNoPayloadCompleter::Sync& completer) override;

  bool received_one_way_no_payload() const { return received_one_way_no_payload_; }

  void ReceivedUnknownMethod(ReceivedUnknownMethodRequest& request,
                             ReceivedUnknownMethodCompleter::Sync& completer) override;

  std::optional<fidl_serversuite::UnknownMethodInfo> received_unknown_method() const {
    return unknown_method_info_;
  }

  void ReceivedStrictOneWay(ReceivedStrictOneWayCompleter::Sync& completer) override;
  bool received_strict_one_way() const { return received_strict_one_way_; }

  void ReceivedFlexibleOneWay(ReceivedFlexibleOneWayCompleter::Sync& completer) override;
  bool received_flexible_one_way() const { return received_flexible_one_way_; }

  void ReplyEncodingFailed(EventEncodingFailedRequest& request,
                           ReplyEncodingFailedCompleter::Sync& completer) override;
  std::optional<fidl_serversuite::EncodingFailureInfo> reply_encoding_failed() const {
    return reply_encoding_failed_;
  }

  void EventEncodingFailed(EventEncodingFailedRequest& request,
                           EventEncodingFailedCompleter::Sync& completer) override;
  std::optional<fidl_serversuite::EncodingFailureInfo> event_encoding_failed() const {
    return event_encoding_failed_;
  }

  void WillTeardown(WillTeardownRequest& request, WillTeardownCompleter::Sync& completer) override;
  std::optional<fidl_serversuite::TeardownReason> teardown_reason() const {
    return teardown_reason_;
  }

 private:
  bool received_one_way_no_payload_ = false;
  std::optional<fidl_serversuite::UnknownMethodInfo> unknown_method_info_;
  bool received_strict_one_way_ = false;
  bool received_flexible_one_way_ = false;
  std::optional<fidl_serversuite::EncodingFailureInfo> reply_encoding_failed_;
  std::optional<fidl_serversuite::EncodingFailureInfo> event_encoding_failed_;
  std::optional<fidl_serversuite::TeardownReason> teardown_reason_;
};

class ServerTest : private ::loop_fixture::RealLoop, public ::testing::Test {
 public:
  static constexpr zx::duration kTimeoutDuration = zx::sec(5);

  explicit ServerTest(fidl_serversuite::Test test, fidl_serversuite::AnyTarget::Tag target_type)
      : test_(test), target_type_(target_type) {}

  void SetUp() override;

  void TearDown() override;

  const Reporter& reporter() { return reporter_; }
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

 private:
  fidl_serversuite::Test test_;

  bool is_teardown_reason_supported_ = false;

  fidl_serversuite::AnyTarget::Tag target_type_;

  fidl::SyncClient<fidl_serversuite::Runner> runner_;

  channel_util::Channel target_;

  Reporter reporter_;
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
