// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdio/directory.h>
#include <lib/scheduler/role.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/provider.h>

#include <filesystem>
#include <system_error>

#include "src/developer/memory/monitor/monitor.h"
#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/log_settings_command_line.h"

namespace {
const char kRamDeviceClassPath[] = "/dev/class/aml-ram";
void SetRamDevice(monitor::Monitor* app) {
  // Look for optional RAM device that provides bandwidth measurement interface.
  fuchsia::hardware::ram::metrics::DevicePtr ram_device;
  std::error_code ec;
  // Use the noexcept version of std::filesystem::exists.
  if (std::filesystem::exists(kRamDeviceClassPath, ec)) {
    for (const auto& entry : std::filesystem::directory_iterator(kRamDeviceClassPath)) {
      zx_status_t status = fdio_service_connect(entry.path().c_str(),
                                                ram_device.NewRequest().TakeChannel().release());
      if (status == ZX_OK) {
        app->SetRamDevice(std::move(ram_device));
        FX_LOGS(INFO) << "Will collect memory bandwidth measurements.";
        return;
      }
      break;
    }
  }
  FX_LOGS(INFO) << "CANNOT collect memory bandwidth measurements. error_code: " << ec;
}
const char kNotfiyCrashReporterPath[] = "/config/data/send_critical_pressure_crash_reports";
bool SendCriticalMemoryPressureCrashReports() {
  std::error_code _ignore;
  return std::filesystem::exists(kNotfiyCrashReporterPath, _ignore);
}
}  // namespace

int main(int argc, const char** argv) {
  auto command_line = fxl::CommandLineFromArgcArgv(argc, argv);
  if (!fxl::SetLogSettingsFromCommandLine(command_line, {"memory_monitor"}))
    return 1;

  FX_LOGS(DEBUG) << argv[0] << ": starting";

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  trace::TraceProviderWithFdio trace_provider(loop.dispatcher(), monitor::Monitor::kTraceName);
  std::unique_ptr<sys::ComponentContext> startup_context =
      sys::ComponentContext::CreateAndServeOutgoingDirectory();

  // Lower the priority.
  zx_status_t status = fuchsia_scheduler::SetRoleForThisThread("fuchsia.memory-monitor.main");
  FX_CHECK(status == ZX_OK) << "Set scheduler role status: " << zx_status_get_string(status);

  monitor::Monitor app(std::move(startup_context), command_line, loop.dispatcher(),
                       true /* send_metrics */, true /* watch_memory_pressure */,
                       SendCriticalMemoryPressureCrashReports(),
                       memory_monitor_config::Config::TakeFromStartupHandle());
  SetRamDevice(&app);
  loop.Run();

  FX_LOGS(DEBUG) << argv[0] << ": exiting";

  return 0;
}
