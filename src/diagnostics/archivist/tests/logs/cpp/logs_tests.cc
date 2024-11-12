// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/logger/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/sys/cpp/service_directory.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/types.h>

#include <memory>
#include <vector>

#include <gtest/gtest.h>
#include <src/lib/fsl/handles/object_info.h>
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

namespace {

class StubLogListener : public fuchsia::logger::LogListenerSafe {
 public:
  using DoneCallback = fit::function<void()>;
  StubLogListener();
  ~StubLogListener() override;

  const std::vector<fuchsia::logger::LogMessage>& GetLogs() { return log_messages_; }

  bool ListenFiltered(const std::shared_ptr<sys::ServiceDirectory>& svc, zx_koid_t pid,
                      const std::string& tag);
  void LogMany(::std::vector<fuchsia::logger::LogMessage> Log, LogManyCallback done) override;
  void Log(fuchsia::logger::LogMessage Log, LogCallback done) override;
  void Done() override;

 private:
  ::fidl::Binding<fuchsia::logger::LogListenerSafe> binding_;
  fuchsia::logger::LogListenerSafePtr log_listener_;
  std::vector<fuchsia::logger::LogMessage> log_messages_;
  DoneCallback done_callback_;
};

StubLogListener::StubLogListener() : binding_(this) { binding_.Bind(log_listener_.NewRequest()); }

StubLogListener::~StubLogListener() {}

void StubLogListener::LogMany(::std::vector<fuchsia::logger::LogMessage> logs,
                              LogManyCallback done) {
  std::move(logs.begin(), logs.end(), std::back_inserter(log_messages_));
  done();
}

void StubLogListener::Log(fuchsia::logger::LogMessage log, LogCallback done) {
  log_messages_.push_back(std::move(log));
  done();
}

void StubLogListener::Done() {
  if (done_callback_) {
    done_callback_();
  }
}

bool StubLogListener::ListenFiltered(const std::shared_ptr<sys::ServiceDirectory>& svc,
                                     zx_koid_t pid, const std::string& tag) {
  if (!log_listener_) {
    return false;
  }
  auto log_service = svc->Connect<fuchsia::logger::Log>();
  auto options = std::make_unique<fuchsia::logger::LogFilterOptions>();
  options->filter_by_pid = true;
  options->pid = pid;
  options->verbosity = 0;
  options->min_severity = fuchsia::logger::LogLevelFilter::TRACE;
  options->tags = {tag};
  log_service->ListenSafe(std::move(log_listener_), std::move(options));
  return true;
}

using LoggerIntegrationTest = gtest::RealLoopFixture;

TEST_F(LoggerIntegrationTest, ListenFiltered) {
  // Make sure there is one syslog message coming from that pid and with a tag
  // unique to this test case.

  auto pid = fsl::GetCurrentProcessKoid();
  auto tag = "logger_integration_cpp_test.ListenFiltered";
  auto message = "my message";
  // severities "in the wild" including both those from the
  // legacy syslog severities and the new.
  std::vector<uint8_t> severities_in_use = {
      fuchsia_logging::LogSeverity::Trace,      // 0x10
      fuchsia_logging::LogSeverity::Debug,      // Legacy "verbosity" (v=1)
      fuchsia_logging::LogSeverity::Info - 10,  // Legacy "verbosity" (v=10)
      fuchsia_logging::LogSeverity::Info - 5,   // Legacy "verbosity" (v=5)
      fuchsia_logging::LogSeverity::Info - 4,   // Legacy "verbosity" (v=4)
      fuchsia_logging::LogSeverity::Info - 3,   // Legacy "verbosity" (v=3)
      fuchsia_logging::LogSeverity::Info,       // 0x30
      fuchsia_logging::LogSeverity::Warn,       // Legacy severity (WARNING)
      fuchsia_logging::LogSeverity::Error,      // 0x50
  };

  // Expected post-transform severities
  std::vector<uint8_t> expected_severities = {
      fuchsia_logging::LogSeverity::Trace,      // 0x10
      fuchsia_logging::LogSeverity::Debug,      // Legacy "verbosity" (v=1)
      fuchsia_logging::LogSeverity::Info - 10,  // Legacy "verbosity" (v=10)
      fuchsia_logging::LogSeverity::Info - 5,   // Legacy "verbosity" (v=5)
      fuchsia_logging::LogSeverity::Info - 4,   // Legacy "verbosity" (v=4)
      fuchsia_logging::LogSeverity::Info - 3,   // Legacy "verbosity" (v=3)
      fuchsia_logging::LogSeverity::Info,       // 0x30
      fuchsia_logging::LogSeverity::Warn,       // Legacy severity (WARNING)
      fuchsia_logging::LogSeverity::Error,      // 0x50
  };
  fuchsia_logging::LogSettingsBuilder builder;
  builder.WithMinLogSeverity(0).WithTags({tag}).BuildAndInitialize();

  for (auto severity : severities_in_use) {
    // Manual expansion of FX_LOGS to support custom severity levels
    (fuchsia_logging::LogMessage(severity, __FILE__, __LINE__, nullptr, nullptr).stream())
        << message;
  }

  // Start the log listener and the logger, and wait for the log message to arrive.
  StubLogListener log_listener;
  ASSERT_TRUE(log_listener.ListenFiltered(sys::ServiceDirectory::CreateFromNamespace(), pid, tag));
  auto& logs = log_listener.GetLogs();
  RunLoopWithTimeoutOrUntil(
      [&logs, expected_size = severities_in_use.size()] { return logs.size() >= expected_size; },
      zx::min(2));

  std::vector<fuchsia::logger::LogMessage> sorted_by_severity(logs.begin(), logs.end());
  std::sort(sorted_by_severity.begin(), sorted_by_severity.end(),
            [](auto a, auto b) { return a.severity < b.severity; });
  ASSERT_EQ(sorted_by_severity.size(), expected_severities.size());
  for (auto i = 0ul; i < logs.size(); i++) {
    ASSERT_EQ(sorted_by_severity[i].tags.size(), 1u);
    EXPECT_EQ(sorted_by_severity[i].tags[0], tag);
    EXPECT_EQ(sorted_by_severity[i].severity, expected_severities[i]);
    EXPECT_EQ(sorted_by_severity[i].pid, pid);
    EXPECT_TRUE(sorted_by_severity[i].msg.find(message) != std::string::npos);
  }
}

}  // namespace
