// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.inspect/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/env.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/provider.h>
#include <lib/vfs/cpp/pseudo_dir.h>
#include <lib/vfs/cpp/service.h>
#include <zircon/status.h>

#include "driver_host.h"
#include "src/devices/lib/log/log.h"
#include "src/sys/lib/stdout-to-debuglog/cpp/stdout-to-debuglog.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

int main(int argc, char** argv) {
  fuchsia_logging::SetTags({"driver_host", "driver"});
  driver_logger::GetLogger().AddTag("driver_host").AddTag("driver");
  // TODO(https://fxbug.dev/42108351): Lock down job.
  zx_status_t status = StdoutToDebuglog::Init();
  if (status != ZX_OK) {
    FX_SLOG(INFO,
            "Failed to redirect stdout to debuglog, assuming test environment and continuing");
  }

  if (zx_status_t status = fdf_env_start(); status != ZX_OK) {
    FX_SLOG(ERROR, "Failed to create the initial dispatcher thread");
    return status;
  }

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  trace::TraceProviderWithFdio trace_provider(loop.dispatcher());

  auto outgoing = component::OutgoingDirectory(loop.dispatcher());

  auto serve = outgoing.ServeFromStartupInfo();
  if (serve.is_error()) {
    FX_SLOG(ERROR, "Failed to serve outgoing directory", FX_KV("status", serve.status_string()));
    return serve.status_value();
  }

  // Setup inspect.
  inspect::ComponentInspector inspector(loop.dispatcher(), {});

  driver_host::DriverHost driver_host(inspector.inspector(), loop);
  auto init = driver_host.PublishDriverHost(outgoing);
  if (init.is_error()) {
    return init.error_value();
  }

  status = loop.Run();
  // All drivers should now be shutdown and stopped.
  // Destroy all dispatchers in case any weren't freed correctly.
  // This will block until all dispatcher callbacks complete.
  fdf_env_destroy_all_dispatchers();
  return status;
}
