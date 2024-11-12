// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_DIAGNOSTICS_ACCESSOR2LOGGER_LOG_MESSAGE_H_
#define SRC_LIB_DIAGNOSTICS_ACCESSOR2LOGGER_LOG_MESSAGE_H_

#include <fuchsia/diagnostics/cpp/fidl.h>
#include <fuchsia/logger/cpp/fidl.h>
#include <lib/fpromise/result.h>
#include <lib/syslog/cpp/log_level.h>

#include <vector>

namespace diagnostics::accessor2logger {

// Prints formatted content to the log.
fpromise::result<std::vector<fpromise::result<fuchsia::logger::LogMessage, std::string>>,
                 std::string>
ConvertFormattedContentToLogMessages(fuchsia::diagnostics::FormattedContent content);

// Get the severity corresponding to the given verbosity. Note that
// verbosity relative to the default severity and can be thought of
// as incrementally "more vebose than" the baseline.
fuchsia_logging::RawLogSeverity GetSeverityFromVerbosity(uint8_t verbosity);

}  // namespace diagnostics::accessor2logger

#endif  // SRC_LIB_DIAGNOSTICS_ACCESSOR2LOGGER_LOG_MESSAGE_H_
