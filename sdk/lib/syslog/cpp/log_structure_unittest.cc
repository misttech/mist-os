// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/logging_backend.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>

#include <cstring>
#include <thread>
#include <vector>

#include <gtest/gtest.h>

#include "src/lib/files/file.h"
#include "src/lib/files/scoped_temp_dir.h"

namespace fuchsia_logging {

TEST(StructuredLogging, Log) {
  FX_SLOG(WARNING, "test_log", FX_KV("foo", "bar"));
  constexpr std::string_view kStringView = "string_view";
  FX_SLOG(WARNING, "test_log", FX_KV("foo", kStringView));
  const std::string kString = "string";
  FX_SLOG(WARNING, "test_log", FX_KV("foo", kString));
  // TODO(fxbug.dev/57482): Figure out how to verify this appropriately.
}

class SideEffectTracker {
 public:
  explicit SideEffectTracker(bool* output) { *output = true; }
  operator int64_t() { return 0; }
};

TEST(StructuredLogging, NoSideEffectsIfLoggingIsDisabled) {
  bool called = false;
  FX_SLOG(DEBUG, "test", FX_KV("a", static_cast<int64_t>(SideEffectTracker(&called))));
  ASSERT_FALSE(called);
  FX_SLOG(INFO, "test", FX_KV("a", static_cast<int64_t>(SideEffectTracker(&called))));
  ASSERT_TRUE(called);
}

template <typename T>
static cpp17::optional<cpp17::string_view> ToStringView(T input) {
  return input;
}

TEST(StructuredLogging, NullSafeStringView) {
  // Construct from nullptr directly.
  ASSERT_EQ(ToStringView(syslog_runtime::NullSafeStringView(nullptr)), cpp17::nullopt);
  // Construct from nullptr via const char*.
  ASSERT_EQ(ToStringView(syslog_runtime::NullSafeStringView(static_cast<const char*>(nullptr))),
            cpp17::nullopt);
  // Construct from std::string
  ASSERT_EQ(ToStringView(syslog_runtime::NullSafeStringView(std::string("test"))),
            cpp17::string_view("test"));
  // Construct from non-null const char*
  ASSERT_EQ(ToStringView(syslog_runtime::NullSafeStringView("test")), cpp17::string_view("test"));
}

// Test to validate that SetLogSettings and log initialization is thread-safe.
TEST(StructuredLogging, ThreadInitialization) {
  // TODO(bbosak): Convert to actual stress test.
  auto start = zx_clock_get_monotonic();
  std::atomic_bool running = true;
  std::thread thread_a([&]() {
    while (running) {
      zx::channel temp[2];
      zx::channel::create(0, &temp[0], &temp[1]);
      syslog_runtime::SetLogSettings(fuchsia_logging::LogSettings{
          .log_sink = temp[0].release(), .wait_for_initial_interest = false});
    }
  });
  std::thread thread_b([&]() {
    while (running) {
      FX_SLOG(WARNING, "test_log", FX_KV("foo", "bar"));
    }
  });
  while (true) {
    auto duration = (zx_clock_get_monotonic() - start);
    if (duration > ZX_SEC(4)) {
      running = false;
      break;
    }

    zx::channel temp[2];
    zx::channel::create(0, &temp[0], &temp[1]);
    syslog_runtime::SetLogSettings(fuchsia_logging::LogSettings{
        .log_sink = temp[0].release(), .wait_for_initial_interest = false});
    FX_SLOG(WARNING, "test_log", FX_KV("foo", "bar"));
  }
  thread_a.join();
  thread_b.join();
  syslog_runtime::SetLogSettings(fuchsia_logging::LogSettings{.wait_for_initial_interest = false});
}

TEST(StructuredLogging, BackendDirect) {
  syslog_runtime::LogBuffer buffer;
  syslog_runtime::BeginRecord(&buffer, fuchsia_logging::LOG_WARNING, "foo.cc", 42, "fake tag",
                              "condition");
  syslog_runtime::FlushRecord(&buffer);
  syslog_runtime::BeginRecord(&buffer, fuchsia_logging::LOG_WARNING, "foo.cc", 42, "fake tag",
                              "condition");
  syslog_runtime::WriteKeyValue(&buffer, "foo", static_cast<int64_t>(42));
  syslog_runtime::WriteKeyValue(&buffer, "bar", true);
  ASSERT_TRUE(syslog_runtime::FlushRecord(&buffer));
  // TODO(fxbug.dev/57482): Figure out how to verify this appropriately.
}

TEST(StructuredLogging, Overflow) {
  std::vector<char> very_large_string;
  very_large_string.resize(1000 * 1000);
  memset(very_large_string.data(), 5, very_large_string.size());
  very_large_string[very_large_string.size() - 1] = 0;
  syslog_runtime::LogBuffer buffer;
  syslog_runtime::BeginRecord(&buffer, fuchsia_logging::LOG_WARNING, "foo.cc", 42, "fake tag",
                              "condition");
  syslog_runtime::FlushRecord(&buffer);
  syslog_runtime::BeginRecord(&buffer, fuchsia_logging::LOG_WARNING, "foo.cc", 42, "fake tag",
                              "condition");
  syslog_runtime::WriteKeyValue(&buffer, "foo", static_cast<int64_t>(42));
  syslog_runtime::WriteKeyValue(&buffer, "bar", very_large_string.data());

  ASSERT_FALSE(syslog_runtime::FlushRecord(&buffer));
}

TEST(StructuredLogging, LOGS) {
  std::string str;
  // 5mb log shouldn't crash
  str.resize(1000 * 5000);
  memset(str.data(), 's', str.size() - 1);
  FX_LOGS(INFO) << str;
}

}  // namespace fuchsia_logging
