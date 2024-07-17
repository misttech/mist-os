// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/fxl/log_settings_command_line.h"

#include <lib/syslog/cpp/macros.h>

#include "lib/syslog/cpp/log_settings.h"
#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/strings/string_number_conversions.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace fxl {

template <typename T>
bool ParseLogSettingsInternal(const fxl::CommandLine& command_line, T* out_settings) {
  T settings = *out_settings;

  // Don't clobber existing settings, but ensure min log level is initialized.
  // Note that legacy INFO level is also 0.
  if (settings.min_log_level == 0) {
    settings.min_log_level = fuchsia_logging::DefaultLogLevel;
  }

  // --severity=<TRACE|DEBUG|INFO|WARNING|ERROR|FATAL>
  std::string severity;
  if (command_line.GetOptionValue("severity", &severity)) {
    fuchsia_logging::LogSeverity level;
    if (severity == "TRACE") {
      level = fuchsia_logging::LOG_TRACE;
    } else if (severity == "DEBUG") {
      level = fuchsia_logging::LOG_DEBUG;
    } else if (severity == "INFO") {
      level = fuchsia_logging::LOG_INFO;
    } else if (severity == "WARNING") {
      level = fuchsia_logging::LOG_WARNING;
    } else if (severity == "ERROR") {
      level = fuchsia_logging::LOG_ERROR;
    } else if (severity == "FATAL") {
      level = fuchsia_logging::LOG_FATAL;
    } else {
      FX_LOGS(ERROR) << "Error parsing --severity option:" << severity;
      return false;
    }

    settings.min_log_level = level;
  }

#ifndef __Fuchsia__
  // --log-file=<file>
  std::string file;
  if (command_line.GetOptionValue("log-file", &file)) {
    settings.log_file = file;
  }
#endif
  // --quiet=<level>
  // Errors out if --severity is present.
  std::string quietness;
  if (command_line.GetOptionValue("quiet", &quietness)) {
    if (!severity.empty()) {
      FX_LOGS(ERROR) << "Setting both --severity and --quiet is not allowed.";
      return false;
    }

    int level = 1;
    if (!quietness.empty() && (!fxl::StringToNumberWithError(quietness, &level) || level < 0)) {
      FX_LOGS(ERROR) << "Error parsing --quiet option: " << quietness;
      return false;
    }
    // Max quiet steps from INFO > WARN > ERROR > FATAL
    if (level > 3) {
      level = 3;
    }
    settings.min_log_level =
        fuchsia_logging::LOG_INFO +
        static_cast<fuchsia_logging::LogSeverity>(level * fuchsia_logging::LogSeverityStepSize);
  }

  *out_settings = settings;
  return true;
}

bool ParseLogSettings(const fxl::CommandLine& command_line, fxl::LogSettings* out_settings) {
  return ParseLogSettingsInternal(command_line, out_settings);
}
#ifdef __Fuchsia__
bool SetLogSettingsFromCommandLine(const fxl::CommandLine& command_line,
                                   async_dispatcher_t* dispatcher) {
#else
bool SetLogSettingsFromCommandLine(const fxl::CommandLine& command_line) {
#endif

  LogSettings settings;
  if (!ParseLogSettings(command_line, &settings))
    return false;
  fuchsia_logging::LogSettingsBuilder builder;
  builder.WithMinLogSeverity(settings.min_log_level);
#ifndef __Fuchsia__
  builder.WithLogFile(settings.log_file);
#else
  if (dispatcher) {
    builder.WithDispatcher(dispatcher);
  }
  if (settings.single_threaded_dispatcher) {
    builder.WithDispatcher(settings.single_threaded_dispatcher);
  }
  if (settings.log_sink) {
    builder.WithLogSink(settings.log_sink);
  }
  if (settings.disable_interest_listener) {
    builder.DisableInterestListener();
  }
  if (!settings.wait_for_initial_interest) {
    builder.DisableWaitForInitialInterest();
  }
#endif
  builder.BuildAndInitialize();
  return true;
}
#ifdef __Fuchsia__
bool SetLogSettingsFromCommandLine(const fxl::CommandLine& command_line,
                                   const std::initializer_list<std::string>& tags,
                                   async_dispatcher_t* dispatcher) {
#else
bool SetLogSettingsFromCommandLine(const fxl::CommandLine& command_line,
                                   const std::initializer_list<std::string>& tags) {
#endif
  LogSettings settings;
  if (!ParseLogSettings(command_line, &settings))
    return false;
  fuchsia_logging::LogSettingsBuilder builder;
  builder.WithMinLogSeverity(settings.min_log_level);
#ifndef __Fuchsia__
  builder.WithLogFile(settings.log_file);
#else
  if (dispatcher) {
    builder.WithDispatcher(dispatcher);
  }
  if (settings.single_threaded_dispatcher) {
    builder.WithDispatcher(settings.single_threaded_dispatcher);
  }
  if (settings.log_sink) {
    builder.WithLogSink(settings.log_sink);
  }
  if (settings.disable_interest_listener) {
    builder.DisableInterestListener();
  }
  if (!settings.wait_for_initial_interest) {
    builder.DisableWaitForInitialInterest();
  }
#endif
  builder.BuildAndInitializeWithTags(tags);
  return true;
}

std::vector<std::string> LogSettingsToArgv(const LogSettings& settings) {
  std::vector<std::string> result;

  if (settings.min_log_level != fuchsia_logging::LOG_INFO) {
    std::string arg;
    if (settings.min_log_level < fuchsia_logging::LOG_INFO &&
        settings.min_log_level > fuchsia_logging::LOG_DEBUG) {
      arg = StringPrintf("--verbose=%d", (fuchsia_logging::LOG_INFO - settings.min_log_level));
    } else if (settings.min_log_level == fuchsia_logging::LOG_TRACE) {
      arg = "--severity=TRACE";
    } else if (settings.min_log_level == fuchsia_logging::LOG_DEBUG) {
      arg = "--severity=DEBUG";
    } else if (settings.min_log_level == fuchsia_logging::LOG_WARNING) {
      arg = "--severity=WARNING";
    } else if (settings.min_log_level == fuchsia_logging::LOG_ERROR) {
      arg = "--severity=ERROR";
    } else {
      arg = "--severity=FATAL";
    }
    result.push_back(arg);
  }

  return result;
}  // namespace fxl

}  // namespace fxl
