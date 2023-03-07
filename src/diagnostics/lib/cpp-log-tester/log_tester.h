// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DIAGNOSTICS_LIB_CPP_LOG_TESTER_LOG_TESTER_H_
#define SRC_DIAGNOSTICS_LIB_CPP_LOG_TESTER_LOG_TESTER_H_
#include <fuchsia/logger/cpp/fidl.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>
#include <stdint.h>

#include <string>

namespace log_tester {

/// Redirects all logs to a fake LogSink
/// and returns the handle to the logsink
/// to be passed to RetrieveLogs later.
zx::channel SetupFakeLog(syslog::LogSettings settings = syslog::LogSettings());

/// Retrieves logs from a given LogSink connection,
/// and converts them to plaintext.
std::string RetrieveLogs(zx::channel remote);

/// Converts logs in the LogSink channel to LogMessages in feedback format.
std::vector<fuchsia::logger::LogMessage> RetrieveLogsAsLogMessage(zx::channel remote);
}  // namespace log_tester

#endif  // SRC_DIAGNOSTICS_LIB_CPP_LOG_TESTER_LOG_TESTER_H_
