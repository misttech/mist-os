// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/syslog/cpp/macros.h>

#include <filesystem>

#include "src/developer/memory/pressure_signaler/debugger.h"
#include "src/developer/memory/pressure_signaler/pressure_notifier.h"

namespace {
const char kNotfiyCrashReporterPath[] = "/config/data/send_critical_pressure_crash_reports";
bool SendCriticalMemoryPressureCrashReports() {
  std::error_code _ignore;
  return std::filesystem::exists(kNotfiyCrashReporterPath, _ignore);
}

}  // namespace

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  std::unique_ptr<sys::ComponentContext> context =
      sys::ComponentContext::CreateAndServeOutgoingDirectory();

  FX_LOGS(DEBUG) << argv[0] << ": starting";
  pressure_signaler::PressureNotifier notifier{/* watch_for_changes = */ true,
                                               SendCriticalMemoryPressureCrashReports(),
                                               context.get(), loop.dispatcher()};
  pressure_signaler::MemoryDebugger debugger{context.get(), &notifier};
  loop.Run();
  FX_LOGS(DEBUG) << argv[0] << ": exiting";

  return 0;
}
