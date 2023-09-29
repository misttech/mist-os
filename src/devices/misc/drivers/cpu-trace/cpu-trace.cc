// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// See the README.md in this directory for documentation.

#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <fidl/fuchsia.perfmon.cpu/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/macros.h>

#include "perf-mon.h"

int main(int argc, char** argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();

  auto client_end = component::Connect<fuchsia_kernel::DebugResource>();
  if (client_end.is_error()) {
    FX_PLOGS(ERROR, client_end.error_value()) << "Failed to get debug resource";
    return -1;
  }
  auto debug_resource_response = fidl::WireSyncClient(std::move(*client_end))->Get();
  if (!debug_resource_response.ok()) {
    FX_PLOGS(ERROR, debug_resource_response.status()) << "Failed to get debug resource";
    return -1;
  }
  zx::resource debug_resource{std::move(debug_resource_response->resource)};

  perfmon::PmuHwProperties props;
  zx_status_t status = perfmon::PerfmonController::GetHwProperties(zx_mtrace_control, &props,
                                                                   debug_resource.borrow());
  if (status != ZX_OK) {
    return -1;
  }

  auto controller = std::make_unique<perfmon::PerfmonController>(props, zx_mtrace_control,
                                                                 debug_resource.borrow());
  zx_status_t init_status = controller->InitOnce();
  if (init_status != ZX_OK) {
    return -1;
  }

  component::OutgoingDirectory outgoing = component::OutgoingDirectory(dispatcher);

  zx::result result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to serve outgoing directory: " << result.status_string();
    return -1;
  }

  const std::string protocol_name = "fuchsia.perfmon.cpu.Controller";
  result =
      outgoing.AddProtocol<fuchsia_perfmon_cpu::Controller>(std::move(controller), protocol_name);
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Failed to add fuchsia.perfmon.cpu.Controller protocol: "
                   << result.status_string();
    return -1;
  }

  loop.Run();
  return EXIT_SUCCESS;
}
