// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYSLOG_CPP_LOG_LEVEL_H_
#define LIB_SYSLOG_CPP_LOG_LEVEL_H_

#include <lib/syslog/structured_backend/fuchsia_syslog.h>

#include <cstdint>

namespace fuchsia_logging {

using LogSeverity = uint8_t;

// Default log levels.
constexpr LogSeverity LOG_TRACE = FUCHSIA_LOG_TRACE;
constexpr LogSeverity LOG_DEBUG = FUCHSIA_LOG_DEBUG;
constexpr LogSeverity LOG_INFO = FUCHSIA_LOG_INFO;
constexpr LogSeverity LOG_WARNING = FUCHSIA_LOG_WARNING;
constexpr LogSeverity LOG_ERROR = FUCHSIA_LOG_ERROR;
constexpr LogSeverity LOG_FATAL = FUCHSIA_LOG_FATAL;

constexpr LogSeverity DefaultLogLevel = LOG_INFO;

constexpr uint8_t LogSeverityStepSize = 0x10;
constexpr uint8_t LogVerbosityStepSize = 0x1;

inline LogSeverity LOG_LEVEL(LogSeverity level) { return level; }

}  // namespace fuchsia_logging

#endif  // LIB_SYSLOG_CPP_LOG_LEVEL_H_
