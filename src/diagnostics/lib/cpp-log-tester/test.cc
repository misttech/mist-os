// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/macros.h>
#include <lib/zx/socket.h>
#include <zircon/types.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>
#include <rapidjson/document.h>
#include <rapidjson/error/en.h>
#include <rapidjson/pointer.h>

#include "log_tester.h"

namespace log_tester {
namespace {

TEST(LogDecoder, DecodesCorrectly) {
  auto channel = SetupFakeLog();
  FX_LOG_KV(INFO, "Test log message", FX_KV("tag", "logging-test"), FX_KV("custom-kvp", 5));
  auto ret = RetrieveLogs(std::move(channel));
  ASSERT_EQ(ret,
            "[src/diagnostics/lib/cpp-log-tester/test.cc(22)] Test log message custom-kvp=5\n");
}

TEST(LogDecoder, DecodesRawMessageCorrectly) {
  auto channel = SetupFakeLog();
  FX_LOG_KV(INFO, "Test log message", FX_KV("tag", "logging-test"), FX_KV("custom-kvp", 5));
  auto ret = RetrieveLogsAsLogMessage(std::move(channel));
  ASSERT_EQ(ret[0].msg,
            "[src/diagnostics/lib/cpp-log-tester/test.cc(30)] Test log message custom-kvp=5");
  ASSERT_EQ(ret[0].tags.size(), static_cast<size_t>(2));
  ASSERT_EQ(ret[0].tags[0], "test_moniker");
  ASSERT_EQ(ret[0].tags[1], "logging-test");
}

}  // namespace
}  // namespace log_tester
