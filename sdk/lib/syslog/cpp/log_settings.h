// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYSLOG_CPP_LOG_SETTINGS_H_
#define LIB_SYSLOG_CPP_LOG_SETTINGS_H_

#include <lib/syslog/cpp/log_level.h>

#include <type_traits>
#ifdef __Fuchsia__
#include <lib/async/dispatcher.h>
#include <lib/zx/channel.h>
#endif

#include <string>

namespace fuchsia_logging {
// Sets the log tags without modifying the settings. This is ignored on host.
void SetTags(const std::initializer_list<std::string>& tags);

// Builder class that allows building log settings,
// then setting them. Eventually, LogSettings will become
// private and not exposed in this library, so all
// callers must go through this builder.
class LogSettingsBuilder {
 public:
  LogSettingsBuilder();
  // Sets the default log severity. If not explicitly set,
  // this defaults to INFO, or to the value specified by Archivist.
  LogSettingsBuilder& WithMinLogSeverity(LogSeverity min_log_level);

  // Disables the interest listener.
  LogSettingsBuilder& DisableInterestListener();

  // Disables waiting for the initial interest from Archivist.
  // The level specified in SetMinLogSeverity or INFO will be used
  // as the default.
  LogSettingsBuilder& DisableWaitForInitialInterest();

#ifndef __Fuchsia__
  // Sets the log file.
  LogSettingsBuilder& WithLogFile(const std::string_view& log_file);

  ~LogSettingsBuilder();
#else
  // Sets the log sink handle.
  LogSettingsBuilder& WithLogSink(zx_handle_t log_sink);

  // Sets the dispatcher to use.
  LogSettingsBuilder& WithDispatcher(async_dispatcher_t* dispatcher);
#endif

  // Configures the log settings with the specified tags
  // and initializes (or re-initializes) the LogSink connection.
  void BuildAndInitializeWithTags(const std::initializer_list<std::string>& tags);

  // Configures the log settings
  // and initializes (or re-initializes) the LogSink connection.
  void BuildAndInitialize();

 private:
#ifdef __Fuchsia__
  uint64_t settings_[3];
#else
  uint64_t settings_[5];
#endif
};

// Gets the minimum log severity for the current process. Never returns a value
// higher than LOG_FATAL.
LogSeverity GetMinLogSeverity();

}  // namespace fuchsia_logging

#endif  // LIB_SYSLOG_CPP_LOG_SETTINGS_H_
