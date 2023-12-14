// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/logging_backend.h>
#include <lib/syslog/cpp/macros.h>

#include <algorithm>
#include <cstring>
#include <iostream>
#include <memory>

#ifdef __Fuchsia__
#include <zircon/status.h>
#endif

namespace fuchsia_logging {
namespace {

const char* StripDots(const char* path) {
  while (strncmp(path, "../", 3) == 0)
    path += 3;
  return path;
}

const char* StripPath(const char* path) {
  auto p = strrchr(path, '/');
  if (p)
    return p + 1;
  else
    return path;
}

}  // namespace

LogMessage::LogMessage(LogSeverity severity, const char* file, int line, const char* condition,
                       const char* tag
#if defined(__Fuchsia__)
                       ,
                       zx_status_t status
#endif
                       )
    : severity_(severity),
      file_(severity_ > LOG_INFO ? StripDots(file) : StripPath(file)),
      line_(line),
      condition_(condition),
      tag_(tag)
#if defined(__Fuchsia__)
      ,
      status_(status)
#endif
{
}

LogMessage::~LogMessage() {
#if defined(__Fuchsia__)
  if (status_ != std::numeric_limits<zx_status_t>::max()) {
    stream_ << ": " << status_ << " (" << zx_status_get_string(status_) << ")";
  }
#endif
  auto buffer = std::make_unique<syslog_runtime::LogBuffer>();
  auto str = stream_.str();
  syslog_runtime::BeginRecord(buffer.get(), severity_, file_, line_, str.data(), condition_);
  if (tag_) {
    syslog_runtime::WriteKeyValue(buffer.get(), "tag", tag_);
  }
  syslog_runtime::FlushRecord(buffer.get());
  if (severity_ >= LOG_FATAL)
    __builtin_debugtrap();
}

bool LogFirstNState::ShouldLog(uint32_t n) {
  const uint32_t counter_value = counter_.fetch_add(1, std::memory_order_relaxed);
  return counter_value < n;
}

bool LogEveryNSecondsState::ShouldLog(uint32_t n) {
  if (ShouldLogInternal(n)) {
    counter_++;
    last_ = GetCurrentTime();
    return true;
  }
  return false;
}

__attribute__((weak)) std::chrono::high_resolution_clock::time_point
LogEveryNSecondsState::GetCurrentTime() {
  return std::chrono::steady_clock::now();
}

bool LogEveryNSecondsState::ShouldLogInternal(uint32_t n) {
  if (counter_ == 0) {
    return true;
  }
  if (std::chrono::duration_cast<std::chrono::seconds>(GetCurrentTime() - last_).count() >= n) {
    return true;
  }
  return false;
}

uint32_t LogEveryNSecondsState::GetCounter() { return counter_; }

uint8_t GetVlogVerbosity() {
  LogSeverity min_level = GetMinLogLevel();
  if (min_level < LOG_INFO && min_level > LOG_DEBUG) {
    return LOG_INFO - min_level;
  }
  return 0;
}

bool ShouldCreateLogMessage(LogSeverity severity) { return severity >= GetMinLogLevel(); }

}  // namespace fuchsia_logging

fuchsia_logging::LogSeverity GetSeverityFromVerbosity(uint8_t verbosity) {
  // Clamp verbosity scale to the interstitial space between INFO and DEBUG
  uint8_t max_verbosity = (fuchsia_logging::LOG_INFO - fuchsia_logging::LOG_DEBUG) /
                          fuchsia_logging::LogVerbosityStepSize;
  if (verbosity > max_verbosity) {
    verbosity = max_verbosity;
  }
  int severity = fuchsia_logging::LOG_INFO - (verbosity * fuchsia_logging::LogVerbosityStepSize);
  if (severity < fuchsia_logging::LOG_DEBUG + 1) {
    return fuchsia_logging::LOG_DEBUG + 1;
  }
  return static_cast<fuchsia_logging::LogSeverity>(severity);
}
