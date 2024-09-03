// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYSLOG_CPP_LOG_LEVEL_H_
#define LIB_SYSLOG_CPP_LOG_LEVEL_H_

#include <lib/syslog/structured_backend/fuchsia_syslog.h>

#include <cstdint>

namespace fuchsia_logging {

using LogSeverity = uint8_t;

constexpr LogSeverity kDefaultLogLevel = FUCHSIA_LOG_INFO;

}  // namespace fuchsia_logging

#endif  // LIB_SYSLOG_CPP_LOG_LEVEL_H_
