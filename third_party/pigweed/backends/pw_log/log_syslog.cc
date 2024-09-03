// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/macros.h>

#include "pw_log_fuchsia/log_fuchsia.h"

extern "C" {
namespace {

::fuchsia_logging::LogSeverity ToFuchsiaLevel(int pw_level) {
  switch (pw_level) {
    case PW_LOG_LEVEL_ERROR:
      return FUCHSIA_LOG_ERROR;
    case PW_LOG_LEVEL_WARN:
      return FUCHSIA_LOG_WARNING;
    case PW_LOG_LEVEL_INFO:
      return FUCHSIA_LOG_INFO;
    case PW_LOG_LEVEL_DEBUG:
      return FUCHSIA_LOG_DEBUG;
    default:
      return FUCHSIA_LOG_ERROR;
  }
}

}  // namespace

void pw_log_fuchsia_impl(int level, const char* module_name, const char* file_name, int line_number,
                         const char* message) {
  ::fuchsia_logging::LogMessage(ToFuchsiaLevel(level), file_name, line_number, nullptr, module_name)
          .stream()
      << message;
}
}
