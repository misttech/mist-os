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
  LogSeverity min_log_level = DefaultLogLevel;
#ifndef __Fuchsia__
  // The name of a file to which the log should be written.
  // When non-empty, the previous log output is closed and logging is
  // redirected to the specified file.  It is not possible to revert to
  // the previous log output through this interface.
  std::string log_file;
#endif
  // Set to true to disable the interest listener. Changes to interest will not be
  // applied to your log settings.
  bool disable_interest_listener = false;
#ifdef __Fuchsia__
  // A single-threaded dispatcher to use for change notifications.
  // Must be single-threaded. Passing a dispatcher that has multiple threads
  // will result in undefined behavior.
  // This must be an async_dispatcher_t*.
  // This can't be defined as async_dispatcher_t* since it is used
  // from fxl which is a host+target library. This prevents us
  // from adding a Fuchsia-specific dependency.
  async_dispatcher_t* single_threaded_dispatcher = nullptr;
  // Allows to define the LogSink handle to use. When no handle is provided, the default LogSink
  // in the pogram incoming namespace will be used.
  zx_handle_t log_sink = ZX_HANDLE_INVALID;
#endif

  // When set to true, it will block log initialization on receiving the initial interest to define
  // the minimum severity.
  bool wait_for_initial_interest = true;
};
static_assert(std::is_copy_constructible<LogSettings>::value);

// Sets the active log settings for the current process.
void SetLogSettings(const LogSettings& settings);

// Sets the active log settings and tags for the current process. |tags| is not
// used on host.
void SetLogSettings(const LogSettings& settings, const std::initializer_list<std::string>& tags);

// Sets the log tags without modifying the settings. This is ignored on host.
void SetTags(const std::initializer_list<std::string>& tags);

// Builder class that allows building log settings,
// then setting them. Eventually, LogSettings will become
// private and not exposed in this library, so all
// callers must go through this builder.
class LogSettingsBuilder {
 public:
  // Sets the default log severity. If not explicitly set,
  // this defaults to INFO, or to the value specified by Archivist.
  LogSettingsBuilder& WithMinLogSeverity(LogSeverity min_log_level) {
    settings_.min_log_level = min_log_level;
    return *this;
  }

  // Disables the interest listener.
  LogSettingsBuilder& DisableInterestListener() {
    settings_.disable_interest_listener = true;
    return *this;
  }

  // Disables waiting for the initial interest from Archivist.
  // The level specified in SetMinLogSeverity or INFO will be used
  // as the default.
  LogSettingsBuilder& DisableWaitForInitialInterest() {
    settings_.wait_for_initial_interest = false;
    return *this;
  }

#ifndef __Fuchsia__
  // Sets the log file.
  LogSettingsBuilder& WithLogFile(const std::string_view& log_file) {
    settings_.log_file = log_file;
    return *this;
  }
#else
  // Sets the log sink handle.
  LogSettingsBuilder& WithLogSink(zx_handle_t log_sink) {
    settings_.log_sink = log_sink;
    return *this;
  }

  // Sets the dispatcher to use.
  LogSettingsBuilder& WithDispatcher(async_dispatcher_t* dispatcher) {
    settings_.single_threaded_dispatcher = dispatcher;
    return *this;
  }
#endif

  // Configures the log settings with the specified tags
  // and initializes (or re-initializes) the LogSink connection.
  void BuildAndInitializeWithTags(const std::initializer_list<std::string>& tags) {
    SetLogSettings(settings_, tags);
  }

  // Configures the log settings
  // and initializes (or re-initializes) the LogSink connection.
  void BuildAndInitialize() { SetLogSettings(settings_); }

 private:
  LogSettings settings_;
};

// Gets the minimum log severity for the current process. Never returns a value
// higher than LOG_FATAL.
LogSeverity GetMinLogSeverity();

}  // namespace fuchsia_logging

#endif  // LIB_SYSLOG_CPP_LOG_SETTINGS_H_
