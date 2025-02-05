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

#include "lib/component/incoming/cpp/protocol.h"
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
  // Lower the priority.
  zx_status_t status = fuchsia_scheduler::SetRoleForThisThread("fuchsia.memory-monitor.main");
  FX_CHECK(status == ZX_OK) << "Set scheduler role status: " << zx_status_get_string(status);

  auto maker_result = memory::CaptureMaker::Create(memory::CreateDefaultOS());
  if (maker_result.is_error()) {
    FX_LOGS(ERROR) << "Error getting capture state: "
                   << zx_status_get_string(maker_result.error_value());
    exit(EXIT_FAILURE);
  }

  std::optional<fidl::Client<fuchsia_memorypressure::Provider>> pressure_provider;
  {
    zx::result client_end = component::Connect<fuchsia_memorypressure::Provider>();
    if (!client_end.is_ok()) {
      FX_LOGS(ERROR) << "Error connecting to FIDL fuchsia.memorypressure.Provider: "
                     << client_end.status_string();
      exit(-1);
    }
    pressure_provider = fidl::Client{std::move(*client_end), loop.dispatcher()};
  }

  std::optional<zx_handle_t> root_job;
  {
    auto client_end = component::Connect<fuchsia_kernel::RootJobForInspect>();
    if (!client_end.is_ok()) {
      FX_LOGS(ERROR) << "Error connecting to root job: " << client_end.error_value();
      exit(-1);
    }

    auto result = fidl::WireCall(*client_end)->Get();
    if (result.status() != ZX_OK) {
      FX_LOGS(ERROR) << "Error getting root job: " << result.status();
      exit(-1);
    }
  }

  std::optional<fidl::SyncClient<fuchsia_metrics::MetricEventLoggerFactory>> factory;
  {
    zx::result client_end = component::Connect<fuchsia_metrics::MetricEventLoggerFactory>();
    if (!client_end.is_ok()) {
      FX_LOGS(ERROR) << "Unable to get metrics.MetricEventLoggerFactory.";
    } else {
      factory.emplace(std::move(client_end.value()));
    }
  }
  auto app = std::make_unique<monitor::Monitor>(
      command_line, loop.dispatcher(), memory_monitor_config::Config::TakeFromStartupHandle(),
      std::move(maker_result.value()), std::move(pressure_provider), root_job, std::move(factory));
  SetRamDevice(app.get());
  component::OutgoingDirectory outgoing = component::OutgoingDirectory(loop.dispatcher());
  zx::result result = outgoing.AddProtocol<fuchsia_memory_inspection::Collector>(std::move(app));
  FX_CHECK(result.is_ok());

  result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve the outgoing directory from startup info: "
                   << result.status_string();
    return -1;
  }

  loop.Run();

  FX_LOGS(DEBUG) << argv[0] << ": exiting";

  return 0;
}
