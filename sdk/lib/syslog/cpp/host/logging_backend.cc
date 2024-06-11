// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <fcntl.h>
#include <inttypes.h>
#include <lib/syslog/cpp/log_level.h>
#include <lib/syslog/cpp/logging_backend.h>
#include <unistd.h>

#include <iostream>
#include <sstream>

#include "lib/syslog/cpp/host/encoder.h"

namespace syslog_runtime {

namespace {
// Settings which control the behavior of logging.
struct LogSettings {
  // The minimum logging level.
  // Anything at or above this level will be logged (if applicable).
  // Anything below this level will be silently ignored.
  //
  // The log level defaults to LOG_INFO.
  //
  // Log messages for FX_VLOGS(x) (from macros.h) log verbosities in
  // the range between INFO and DEBUG
  fuchsia_logging::LogSeverity min_log_level = fuchsia_logging::LOG_INFO;
  // The name of a file to which the log should be written.
  // When non-empty, the previous log output is closed and logging is
  // redirected to the specified file.  It is not possible to revert to
  // the previous log output through this interface.
  std::string log_file;
  // Set to true to disable the interest listener. Changes to interest will not be
  // applied to your log settings.
  bool disable_interest_listener = false;

  // When set to true, it will block log initialization on receiving the initial interest to define
  // the minimum severity.
  bool wait_for_initial_interest = true;
};
static_assert(std::is_copy_constructible<LogSettings>::value);
// It's OK to keep global state here even though this file is in a source_set because on host
// we don't use shared libraries.
LogSettings g_log_settings;

}  // namespace

const std::string GetNameForLogSeverity(fuchsia_logging::LogSeverity severity) {
  switch (severity) {
    case fuchsia_logging::LOG_TRACE:
      return "TRACE";
    case fuchsia_logging::LOG_DEBUG:
      return "DEBUG";
    case fuchsia_logging::LOG_INFO:
      return "INFO";
    case fuchsia_logging::LOG_WARNING:
      return "WARNING";
    case fuchsia_logging::LOG_ERROR:
      return "ERROR";
    case fuchsia_logging::LOG_FATAL:
      return "FATAL";
  }

  if (severity > fuchsia_logging::LOG_DEBUG && severity < fuchsia_logging::LOG_INFO) {
    std::ostringstream stream;
    stream << "VLOG(" << (fuchsia_logging::LOG_INFO - severity) << ")";
    return stream.str();
  }

  return "UNKNOWN";
}

void SetLogSettings(const syslog_runtime::LogSettings& settings) {
  g_log_settings.min_log_level = std::min(fuchsia_logging::LOG_FATAL, settings.min_log_level);

  if (g_log_settings.log_file != settings.log_file) {
    if (!settings.log_file.empty()) {
      int fd = open(settings.log_file.c_str(), O_WRONLY | O_CREAT | O_APPEND, S_IRUSR | S_IWUSR);
      if (fd < 0) {
        std::cerr << "Could not open log file: " << settings.log_file << " (" << strerror(errno)
                  << ")" << std::endl;
      } else {
        // Redirect stderr to file.
        if (dup2(fd, STDERR_FILENO) < 0) {
          std::cerr << "Could not set stderr to log file: " << settings.log_file << " ("
                    << strerror(errno) << ")" << std::endl;
        } else {
          g_log_settings.log_file = settings.log_file;
        }
        close(fd);
      }
    }
  }
}

void SetLogSettings(const syslog_runtime::LogSettings& settings,
                    const std::initializer_list<std::string>& tags) {
  syslog_runtime::SetLogSettings(settings);
}

void SetLogTags(const std::initializer_list<std::string>& tags) {
  // Global tags aren't supported on host.
}

fuchsia_logging::LogSeverity GetMinLogSeverity() {
  return syslog_runtime::g_log_settings.min_log_level;
}

void BeginRecord(LogBuffer* buffer, fuchsia_logging::LogSeverity severity, NullSafeStringView file,
                 unsigned int line, NullSafeStringView msg, NullSafeStringView condition) {
  BeginRecordLegacy(buffer, severity, file, line, msg, condition);
}

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, cpp17::string_view value) {
  WriteKeyValueLegacy(buffer, key, value);
}

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, int64_t value) {
  WriteKeyValueLegacy(buffer, key, value);
}

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, uint64_t value) {
  WriteKeyValueLegacy(buffer, key, value);
}

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, double value) {
  WriteKeyValueLegacy(buffer, key, value);
}

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, bool value) {
  WriteKeyValueLegacy(buffer, key, value);
}

void EndRecord(LogBuffer* buffer) { EndRecordLegacy(buffer); }

bool LogBuffer::Flush() {
  auto header = MsgHeader::CreatePtr(this);
  *(header->offset++) = 0;
  if (header->user_tag) {
    auto tag = header->user_tag;
    std::cerr << "[" << tag << "] ";
  }
  std::cerr << reinterpret_cast<const char*>(this->data) << std::endl;
  return true;
}

void WriteLog(fuchsia_logging::LogSeverity severity, const char* file, unsigned int line,
              const char* tag, const char* condition, const std::string& msg) {
  if (tag)
    std::cerr << "[" << tag << "] ";

  std::cerr << "[" << GetNameForLogSeverity(severity) << ":" << file << "(" << line << ")]";

  if (condition)
    std::cerr << " Check failed: " << condition << ".";

  std::cerr << msg << std::endl;
  std::cerr.flush();
}

}  // namespace syslog_runtime

namespace fuchsia_logging {
static_assert(sizeof(LogSettingsBuilder) >= sizeof(syslog_runtime::LogSettings));
static_assert(std::alignment_of_v<LogSettingsBuilder> >=
              std::alignment_of_v<syslog_runtime::LogSettings>);
syslog_runtime::LogSettings& GetSettings(uint64_t* value) {
  return *reinterpret_cast<syslog_runtime::LogSettings*>(value);
}

// Sets the default log severity. If not explicitly set,
// this defaults to INFO, or to the value specified by Archivist.
LogSettingsBuilder& LogSettingsBuilder::WithMinLogSeverity(LogSeverity min_log_level) {
  GetSettings(settings_).min_log_level = min_log_level;
  return *this;
}

// Disables the interest listener.
LogSettingsBuilder& LogSettingsBuilder::DisableInterestListener() {
  GetSettings(settings_).disable_interest_listener = true;
  return *this;
}

fuchsia_logging::LogSeverity GetMinLogSeverity() {
  return syslog_runtime::g_log_settings.min_log_level;
}

// Disables waiting for the initial interest from Archivist.
// The level specified in SetMinLogSeverity or INFO will be used
// as the default.
LogSettingsBuilder& LogSettingsBuilder::DisableWaitForInitialInterest() {
  GetSettings(settings_).wait_for_initial_interest = false;
  return *this;
}

// Sets the log file.
LogSettingsBuilder& LogSettingsBuilder::WithLogFile(const std::string_view& log_file) {
  GetSettings(settings_).log_file = log_file;
  return *this;
}

LogSettingsBuilder::LogSettingsBuilder() { new (settings_) syslog_runtime::LogSettings(); }

LogSettingsBuilder::~LogSettingsBuilder() { GetSettings(settings_).~LogSettings(); }

// Configures the log settings with the specified tags.
void LogSettingsBuilder::BuildAndInitializeWithTags(
    const std::initializer_list<std::string>& tags) {
  syslog_runtime::SetLogSettings(GetSettings(settings_), tags);
}

// Configures the log settings.
void LogSettingsBuilder::BuildAndInitialize() {
  syslog_runtime::SetLogSettings(GetSettings(settings_));
}

}  // namespace fuchsia_logging
