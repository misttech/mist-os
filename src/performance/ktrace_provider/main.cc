// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/provider.h>

#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/log_settings_command_line.h"
#include "src/performance/ktrace_provider/app.h"

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto command_line = fxl::CommandLineFromArgcArgv(argc, argv);
  if (!fxl::SetLogSettingsFromCommandLine(command_line))
    return 1;

  trace::TraceProviderWithFdio trace_provider(loop.dispatcher(), "ktrace_provider");
  trace_provider.SetGetKnownCategoriesCallback(ktrace_provider::GetKnownCategories);

  auto tracing_client_end = component::Connect<fuchsia_kernel::TracingResource>();
  if (tracing_client_end.is_error()) {
    FX_PLOGS(ERROR, tracing_client_end.error_value())
        << "Failed to get connect to tracing resource";
    return 1;
  }
  auto tracing_result = fidl::SyncClient(std::move(*tracing_client_end))->Get();
  if (!tracing_result.is_ok()) {
    FX_LOGS(ERROR) << tracing_result.error_value() << " Failed to get tracing resource";
    return 1;
  }

  ktrace_provider::App app(std::move(tracing_result->resource()), command_line);
  loop.Run();
  return 0;
}
