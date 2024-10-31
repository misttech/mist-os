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
#include <memory>
#include <system_error>

#include "src/developer/memory/metrics/capture.h"
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
}  // namespace

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto command_line = fxl::CommandLineFromArgcArgv(argc, argv);
  if (!fxl::SetLogSettingsFromCommandLine(command_line, {"memory_monitor"}))
    return 1;

  FX_LOGS(DEBUG) << argv[0] << ": starting";

  trace::TraceProviderWithFdio trace_provider(loop.dispatcher(), monitor::Monitor::kTraceName);
  std::unique_ptr<sys::ComponentContext> startup_context =
      sys::ComponentContext::CreateAndServeOutgoingDirectory();

  // Lower the priority.
  zx_status_t status = fuchsia_scheduler::SetRoleForThisThread("fuchsia.memory-monitor.main");
  FX_CHECK(status == ZX_OK) << "Set scheduler role status: " << zx_status_get_string(status);

  auto maker_result = memory::CaptureMaker::Create(memory::CreateDefaultOS());
  if (maker_result.is_error()) {
    FX_LOGS(ERROR) << "Error getting capture state: "
                   << zx_status_get_string(maker_result.error_value());
    exit(EXIT_FAILURE);
  }

  monitor::Monitor app(std::move(startup_context), command_line, loop.dispatcher(),
                       true /* send_metrics */, true /* watch_memory_pressure */,
                       memory_monitor_config::Config::TakeFromStartupHandle(),
                       std::move(maker_result.value()));
  SetRamDevice(&app);
  loop.Run();

  FX_LOGS(DEBUG) << argv[0] << ": exiting";

  return 0;
}
