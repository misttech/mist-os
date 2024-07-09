// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/logger/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>

#include <algorithm>
#include <iostream>
#include <iterator>
#include <string>

#include "src/ui/bin/system_monitor/system_monitor.h"

int main(int argc, const char** argv) {
  system_monitor::SystemMonitor systemMonitor;
  std::string target("platform_metrics");
  const std::vector<std::string>& recentDiagnos = systemMonitor.updateRecentDiagnostic();
  for (auto& content : recentDiagnos) {
    std::size_t found = content.find(target);
    if (found != std::string::npos) {
      FX_LOGS(INFO) << "Recent Diagnostics: " << content;
    }
  }

  return 0;
}
