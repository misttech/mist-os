// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/logging_backend.h>

namespace fuchsia_logging {

void SetLogSettings(const LogSettings& settings) { syslog_runtime::SetLogSettings(settings); }

void SetLogSettings(const LogSettings& settings, const std::initializer_list<std::string>& tags) {
  syslog_runtime::SetLogSettings(settings, tags);
}

void SetTags(const std::initializer_list<std::string>& tags) {
  SetLogSettings({.min_log_level = GetMinLogLevel()}, tags);
}

LogSeverity GetMinLogLevel() { return syslog_runtime::GetMinLogLevel(); }

}  // namespace fuchsia_logging
