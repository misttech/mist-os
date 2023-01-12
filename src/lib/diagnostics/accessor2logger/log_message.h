// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_DIAGNOSTICS_ACCESSOR2LOGGER_LOG_MESSAGE_H_
#define SRC_LIB_DIAGNOSTICS_ACCESSOR2LOGGER_LOG_MESSAGE_H_

#include <fuchsia/diagnostics/cpp/fidl.h>
#include <fuchsia/logger/cpp/fidl.h>
#include <lib/fpromise/result.h>

#include <vector>

namespace diagnostics::accessor2logger {

// Convenience method that calls the function below with |use_host_encoding| to false.
fpromise::result<std::vector<fpromise::result<fuchsia::logger::LogMessage, std::string>>,
                 std::string>
ConvertFormattedContentToLogMessages(fuchsia::diagnostics::FormattedContent content);

// Prints formatted content to the log. If |use_host_encoding| is true, then use necessary escape
// sequences for log messages on the host.
fpromise::result<std::vector<fpromise::result<fuchsia::logger::LogMessage, std::string>>,
                 std::string>
ConvertFormattedContentToLogMessages(fuchsia::diagnostics::FormattedContent content,
                                     bool use_host_encoding);

}  // namespace diagnostics::accessor2logger

#endif  // SRC_LIB_DIAGNOSTICS_ACCESSOR2LOGGER_LOG_MESSAGE_H_
