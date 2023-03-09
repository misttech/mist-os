// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/logging_backend.h>
#include <lib/syslog/cpp/macros.h>

#include <functional>
#include <mutex>
#include <optional>
#include <string>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "lib/syslog/cpp/log_level.h"
#include "logging_backend_shared.h"
#include "src/lib/files/file.h"
#include "src/lib/files/scoped_temp_dir.h"
#include "src/lib/uuid/uuid.h"

#ifdef __Fuchsia__
#include <fuchsia/diagnostics/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/dispatcher.h>
#include <lib/async/wait.h>
#include <lib/fpromise/promise.h>
#include <lib/zx/socket.h>

#include <cinttypes>

#include <rapidjson/document.h>
#include <src/diagnostics/lib/cpp-log-tester/log_tester.h>

#include "fuchsia/logger/cpp/fidl.h"
#include "src/lib/fxl/strings/join_strings.h"
#include "src/lib/fxl/strings/string_printf.h"
#endif

namespace syslog {
namespace {

class LoggingFixture : public ::testing::Test {
 public:
  LoggingFixture() : old_severity_(GetMinLogLevel()), old_stderr_(dup(STDERR_FILENO)) {}
  ~LoggingFixture() {
    SetLogSettings({.min_log_level = old_severity_, .wait_for_initial_interest = true});
  }

 private:
  LogSeverity old_severity_;
  int old_stderr_;
};

using LoggingFixtureDeathTest = LoggingFixture;
#ifdef __Fuchsia__
std::string SeverityToString(const int32_t severity) {
  if (severity == syslog::LOG_TRACE) {
    return "TRACE";
  } else if (severity == syslog::LOG_DEBUG) {
    return "DEBUG";
  } else if (severity > syslog::LOG_DEBUG && severity < syslog::LOG_INFO) {
    return fxl::StringPrintf("VLOG(%d)", syslog::LOG_INFO - severity);
  } else if (severity == syslog::LOG_INFO) {
    return "INFO";
  } else if (severity == syslog::LOG_WARNING) {
    return "WARN";
  } else if (severity == syslog::LOG_ERROR) {
    return "ERROR";
  } else if (severity == syslog::LOG_FATAL) {
    return "FATAL";
  }
  return "INVALID";
}

std::string Format(const fuchsia::logger::LogMessage& message) {
  return fxl::StringPrintf("[%05d.%03d][%05" PRIu64 "][%05" PRIu64 "][%s] %s: %s\n",
                           static_cast<int>(message.time / 1000000000ULL),
                           static_cast<int>((message.time / 1000000ULL) % 1000ULL), message.pid,
                           message.tid, fxl::JoinStrings(message.tags, ", ").c_str(),
                           SeverityToString(message.severity).c_str(), message.msg.c_str());
}

static std::string RetrieveLogs(zx::channel channel) {
  auto logs = log_tester::RetrieveLogsAsLogMessage(std::move(channel));
  std::stringstream stream;
  for (const auto& log : logs) {
    stream << Format(log);
  }
  return stream.str();
}
#endif

#ifdef __Fuchsia__
using LogState = zx::channel;
static LogState SetupLogs(LogSettings new_settings = LogSettings()) {
  return log_tester::SetupFakeLog(new_settings);
}

static std::string ReadLogs(zx::channel& remote) { return RetrieveLogs(std::move(remote)); }
#else

struct TestLogState {
  files::ScopedTempDir temp_dir;
  std::string log_file;
};

using LogState = std::unique_ptr<TestLogState>;

static LogState SetupLogs(LogSettings new_settings = LogSettings()) {
  auto state = std::make_unique<TestLogState>();
  state->temp_dir.NewTempFile(&new_settings.log_file);
  state->log_file = new_settings.log_file;
  SetLogSettings(new_settings);
  return state;
}

static std::string ReadLogs(LogState& state) {
  std::string log;
  files::ReadFileToString(state->log_file, &log);
  return log;
}
#endif

TEST_F(LoggingFixture, Log) {
  LogState state = SetupLogs();

  int error_line = __LINE__ + 1;
  FX_LOGS(ERROR) << "something at error";

  int info_line = __LINE__ + 1;
  FX_LOGS(INFO) << "and some other at info level";

  std::string log = ReadLogs(state);

  EXPECT_THAT(log, testing::HasSubstr("ERROR: [sdk/lib/syslog/cpp/logging_unittest.cc(" +
                                      std::to_string(error_line) + ")] something at error"));

  EXPECT_THAT(log, testing::HasSubstr("INFO: [logging_unittest.cc(" + std::to_string(info_line) +
                                      ")] and some other at info level"));
}

TEST_F(LoggingFixture, LogFirstN) {
  constexpr int kLimit = 5;
  constexpr int kCycles = 20;
  constexpr const char* kLogMessage = "Hello";
  static_assert(kCycles > kLimit);

  LogState state = SetupLogs();

  for (int i = 0; i < kCycles; ++i) {
    FX_LOGS_FIRST_N(ERROR, kLimit) << kLogMessage;
  }

  std::string log = ReadLogs(state);

  int count = 0;
  size_t pos = 0;
  while ((pos = log.find(kLogMessage, pos)) != std::string::npos) {
    ++count;
    ++pos;
  }
  EXPECT_EQ(kLimit, count);
}

TEST_F(LoggingFixture, LogT) {
  LogState state = SetupLogs();

  int error_line = __LINE__ + 1;
  FX_LOGST(ERROR, "first") << "something at error";

  int info_line = __LINE__ + 1;
  FX_LOGST(INFO, "second") << "and some other at info level";

  std::string log = ReadLogs(state);

  EXPECT_THAT(log, testing::HasSubstr("first] ERROR: [sdk/lib/syslog/cpp/logging_unittest.cc(" +
                                      std::to_string(error_line) + ")] something at error"));

  EXPECT_THAT(log,
              testing::HasSubstr("second] INFO: [logging_unittest.cc(" + std::to_string(info_line) +
                                 ")] and some other at info level"));
}

TEST_F(LoggingFixture, VLogT) {
  LogSettings new_settings;
  new_settings.min_log_level = (LOG_INFO - 2);  // verbosity = 2
  new_settings.wait_for_initial_interest = false;
  LogState state = SetupLogs(new_settings);

  int line = __LINE__ + 1;
  FX_VLOGST(1, "first") << "First message";
  FX_VLOGST(2, "second") << "ABCD";
  FX_VLOGST(3, "third") << "EFGH";

  std::string log = ReadLogs(state);
  EXPECT_THAT(log, testing::HasSubstr("[first] VLOG(1): [logging_unittest.cc(" +
                                      std::to_string(line) + ")] First message"));
  EXPECT_THAT(log, testing::HasSubstr("second"));
  EXPECT_THAT(log, testing::HasSubstr("ABCD"));

  EXPECT_THAT(log, testing::Not(testing::HasSubstr("third")));
  EXPECT_THAT(log, testing::Not(testing::HasSubstr("EFGH")));
}

TEST_F(LoggingFixture, VlogVerbosity) {
  LogSettings new_settings;
  new_settings.wait_for_initial_interest = false;
  EXPECT_EQ(LOG_INFO, new_settings.min_log_level);

  EXPECT_EQ(0, GetVlogVerbosity());

  new_settings.min_log_level = LOG_INFO - 1;
  LogState state = SetupLogs(new_settings);

  EXPECT_EQ(1, GetVlogVerbosity());

  new_settings.min_log_level = LOG_INFO - 15;
  state = SetupLogs(new_settings);

  EXPECT_EQ(15, GetVlogVerbosity());

  new_settings.min_log_level = LOG_DEBUG;
  state = SetupLogs(new_settings);

  EXPECT_EQ(0, GetVlogVerbosity());
}

TEST_F(LoggingFixture, DVLogNoMinLevel) {
  LogSettings new_settings;
  new_settings.wait_for_initial_interest = false;
  EXPECT_EQ(LOG_INFO, new_settings.min_log_level);
  LogState state = SetupLogs(new_settings);

  FX_DVLOGS(1) << "hello";

  std::string log = ReadLogs(state);
  EXPECT_EQ(log, "");
}

TEST_F(LoggingFixture, DVLogWithMinLevel) {
  LogSettings new_settings;
  new_settings.wait_for_initial_interest = false;
  EXPECT_EQ(LOG_INFO, new_settings.min_log_level);
  new_settings.min_log_level = (LOG_INFO - 1);
  LogState state = SetupLogs(new_settings);

  FX_DVLOGS(1) << "hello";

  std::string log = ReadLogs(state);
#if defined(NDEBUG)
  EXPECT_EQ(log, "");
#else
  EXPECT_THAT(log, testing::HasSubstr("hello"));
#endif
}

TEST_F(LoggingFixtureDeathTest, CheckFailed) { ASSERT_DEATH(FX_CHECK(false), ""); }

#if defined(__Fuchsia__)
TEST_F(LoggingFixture, Plog) {
  LogSettings new_settings;
  new_settings.wait_for_initial_interest = false;
  EXPECT_EQ(LOG_INFO, new_settings.min_log_level);
  auto remote = log_tester::SetupFakeLog(new_settings);

  FX_PLOGS(ERROR, ZX_OK) << "should be ok";
  FX_PLOGS(ERROR, ZX_ERR_ACCESS_DENIED) << "got access denied";

  std::string log = RetrieveLogs(std::move(remote));

  EXPECT_THAT(log, testing::HasSubstr("should be ok: 0 (ZX_OK)"));
  EXPECT_THAT(log, testing::HasSubstr("got access denied: -30 (ZX_ERR_ACCESS_DENIED)"));
}

TEST_F(LoggingFixture, PlogT) {
  LogSettings new_settings;
  new_settings.wait_for_initial_interest = false;
  EXPECT_EQ(LOG_INFO, new_settings.min_log_level);
  auto remote = log_tester::SetupFakeLog(new_settings);

  int line1 = __LINE__ + 1;
  FX_PLOGST(ERROR, "abcd", ZX_OK) << "should be ok";

  int line2 = __LINE__ + 1;
  FX_PLOGST(ERROR, "qwerty", ZX_ERR_ACCESS_DENIED) << "got access denied";

  std::string log = RetrieveLogs(std::move(remote));

  EXPECT_THAT(log, testing::HasSubstr("abcd] ERROR: [sdk/lib/syslog/cpp/logging_unittest.cc(" +
                                      std::to_string(line1) + ")] should be ok: 0 (ZX_OK)"));
  EXPECT_THAT(log, testing::HasSubstr("qwerty] ERROR: [sdk/lib/syslog/cpp/logging_unittest.cc(" +
                                      std::to_string(line2) +
                                      ")] got access denied: -30 (ZX_ERR_ACCESS_DENIED)"));
}
#endif  // defined(__Fuchsia__)

TEST_F(LoggingFixture, SLog) {
  LogSettings new_settings;
  new_settings.wait_for_initial_interest = false;
  EXPECT_EQ(LOG_INFO, new_settings.min_log_level);
  LogState state = SetupLogs(new_settings);
  std::string log_id = uuid::Generate();

  int line1 = __LINE__ + 1;
  FX_SLOG(ERROR, nullptr, "some_msg", "String log");

  int line2 = __LINE__ + 1;
  FX_SLOG(ERROR, nullptr, "some_msg", 42);

  int line4 = __LINE__ + 1;
  FX_SLOG(ERROR, "msg", "first", 42, "second", "string");

  int line5 = __LINE__ + 1;
  FX_SLOG(ERROR, "String log");

  int line6 = __LINE__ + 1;
  FX_SLOG(ERROR, nullptr, "float", 0.25f);

  int line7 = __LINE__ + 1;
  FX_SLOG(ERROR, "String with quotes", "value", "char is '\"'");

  std::string log = ReadLogs(state);
  EXPECT_THAT(
      log, testing::HasSubstr("ERROR: [" + std::string("sdk/lib/syslog/cpp/logging_unittest.cc") +
                              "(" + std::to_string(line1) + ")] some_msg=\"String log\""));
  EXPECT_THAT(
      log, testing::HasSubstr("ERROR: [" + std::string("sdk/lib/syslog/cpp/logging_unittest.cc") +
                              "(" + std::to_string(line2) + ")] some_msg=42"));
  EXPECT_THAT(
      log, testing::HasSubstr("ERROR: [" + std::string("sdk/lib/syslog/cpp/logging_unittest.cc") +
                              "(" + std::to_string(line4) + ")] msg first=42 second=\"string\""));
  EXPECT_THAT(
      log, testing::HasSubstr("ERROR: [" + std::string("sdk/lib/syslog/cpp/logging_unittest.cc") +
                              "(" + std::to_string(line5) + ")] String log"));
  EXPECT_THAT(
      log, testing::HasSubstr("ERROR: [" + std::string("sdk/lib/syslog/cpp/logging_unittest.cc") +
                              "(" + std::to_string(line6) + ")] float=0.25"));

  EXPECT_THAT(log, testing::HasSubstr(
                       "ERROR: [" + std::string("sdk/lib/syslog/cpp/logging_unittest.cc") + "(" +
                       std::to_string(line7) + ")] String with quotes value=\"char is '\\\"'\""));
}

TEST_F(LoggingFixture, BackendDirect) {
  LogSettings new_settings;
  new_settings.wait_for_initial_interest = false;
  EXPECT_EQ(LOG_INFO, new_settings.min_log_level);
  LogState state = SetupLogs(new_settings);

  syslog_backend::LogBuffer buffer;
  syslog_backend::BeginRecord(&buffer, syslog::LOG_ERROR, "foo.cc", 42, "Log message", "condition");
  syslog_backend::WriteKeyValue(&buffer, "tag", "fake tag");
  syslog_backend::EndRecord(&buffer);
  syslog_backend::FlushRecord(&buffer);
  syslog_backend::BeginRecord(&buffer, syslog::LOG_ERROR, "foo.cc", 42, "fake message",
                              "condition");
  syslog_backend::WriteKeyValue(&buffer, "tag", "fake tag");
  syslog_backend::WriteKeyValue(&buffer, "foo", static_cast<int64_t>(42));
  syslog_backend::EndRecord(&buffer);
  syslog_backend::FlushRecord(&buffer);

  std::string log = ReadLogs(state);
  EXPECT_THAT(log,
              testing::HasSubstr("ERROR: [foo.cc(42)] Check failed: condition. Log message\n"));
  EXPECT_THAT(log, testing::HasSubstr(
                       "ERROR: [foo.cc(42)] Check failed: condition. fake message foo=42\n"));
}

TEST_F(LoggingFixture, MacroCompilationTest) {
  uint8_t zero = 0;
  FX_SLOG(DEBUG, "test log", KV("key", static_cast<uint16_t>(zero)));
  FX_SLOG(DEBUG, "test log", KV("key", static_cast<uint32_t>(zero)));
  FX_SLOG(DEBUG, "test log", KV("key", static_cast<uint64_t>(zero)));
  FX_SLOG(DEBUG, "test log", KV("key", static_cast<size_t>(zero)));

  FX_SLOG(DEBUG, "test log", KV("key", static_cast<int16_t>(zero)));
  FX_SLOG(DEBUG, "test log", KV("key", static_cast<int32_t>(zero)));
  FX_SLOG(DEBUG, "test log", KV("key", static_cast<int64_t>(zero)));
}

TEST(StructuredLogging, LOGS) {
  std::string str;
  // 5mb log shouldn't crash
  str.resize(1000 * 5000);
  memset(str.data(), 's', str.size() - 1);
  FX_LOGS(INFO) << str;
}

#ifndef __Fuchsia__
// Fuchsia no longer uses logging_backend_shared
// since all logs are now structured.
TEST(StructuredLogging, Remaining) {
  LogSettings new_settings;
  new_settings.wait_for_initial_interest = false;
  files::ScopedTempDir temp_dir;
  ASSERT_TRUE(temp_dir.NewTempFile(&new_settings.log_file));
  SetLogSettings(new_settings);
  syslog_backend::LogBuffer buffer;
  syslog_backend::BeginRecord(&buffer, LOG_INFO, "test", 5, "test_msg", "");
  auto header = syslog_backend::MsgHeader::CreatePtr(&buffer);
  auto initial = header->RemainingSpace();
  header->WriteChar('t');
  ASSERT_EQ(header->RemainingSpace(), initial - 1);
  header->WriteString("est");
  ASSERT_EQ(header->RemainingSpace(), initial - 4);
}

TEST(StructuredLogging, FlushAndReset) {
  syslog_backend::LogBuffer buffer;
  syslog_backend::BeginRecord(&buffer, LOG_INFO, "test", 5, "test_msg", "");
  auto header = syslog_backend::MsgHeader::CreatePtr(&buffer);
  auto initial = header->RemainingSpace();
  header->WriteString("test");
  ASSERT_EQ(header->RemainingSpace(), initial - 4);
  header->FlushAndReset();
  ASSERT_EQ(header->RemainingSpace(),
            sizeof(syslog_backend::LogBuffer::data) - 2);  // last byte reserved for NULL terminator
}
#endif

}  // namespace
}  // namespace syslog
