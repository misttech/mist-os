// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/debug.h>

#include "pw_log_fuchsia/log_fuchsia.h"

namespace {
inline fx_log_severity_t LogLevelToDdkLog(int level) {
  switch (level) {
    case PW_LOG_LEVEL_DEBUG:
      return DDK_LOG_DEBUG;
    case PW_LOG_LEVEL_INFO:
      return DDK_LOG_INFO;
    case PW_LOG_LEVEL_WARN:
      return DDK_LOG_WARNING;
    case PW_LOG_LEVEL_ERROR:
      return DDK_LOG_ERROR;
    case PW_LOG_LEVEL_CRITICAL:
      return DDK_LOG_ERROR;
    default:
      return DDK_LOG_INFO;
  }
}
}  // namespace

extern "C" {
void pw_log_fuchsia_impl(int level, const char* module_name, const char* file_name, int line_number,
                         const char* message) {
  fx_log_severity_t severity = LogLevelToDdkLog(level);
  if (driver_log_severity_enabled_internal(__zircon_driver_rec__.driver, severity)) {
    driver_logf_internal(__zircon_driver_rec__.driver, severity, module_name, file_name,
                         line_number, "%s", message);
  }
}
}
