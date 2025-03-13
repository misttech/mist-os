// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/scheduler/role.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/provider.h>
#include <lib/zx/thread.h>

#include <memory>

#include "lib/inspect/cpp/inspect.h"
#include "src/graphics/display/lib/coordinator-getter/client.h"
#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/log_settings_command_line.h"
#include "src/ui/scenic/bin/app.h"

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  auto command_line = fxl::CommandLineFromArgcArgv(argc, argv);
  if (!fxl::SetLogSettingsFromCommandLine(command_line, {"scenic"}))
    return 1;

  trace::TraceProviderWithFdio trace_provider(loop.dispatcher());
  // This call creates ComponentContext, but does not start serving immediately. Outgoing directory
  // is served by App, after App::InitializeServices() is completed.
  std::unique_ptr<sys::ComponentContext> app_context = sys::ComponentContext::Create();

  // Set up an inspect::Node to inject into the App.
  inspect::ComponentInspector inspector(loop.dispatcher(), {});

  auto display_coordinator_promise = display::GetCoordinator();

  // Instantiate Scenic app.
  scenic_impl::App app(std::move(app_context), inspector.root().CreateChild("scenic"),
                       std::move(display_coordinator_promise), [&loop] { loop.Quit(); });

  // Apply the scheduler role defined for Scenic.
  const zx_status_t thread_status = fuchsia_scheduler::SetRoleForThisThread("fuchsia.scenic.main");
  if (thread_status != ZX_OK) {
    FX_LOGS(WARNING) << "Failed to apply profile to main thread: " << thread_status;
  }
  const zx_status_t vmar_status = fuchsia_scheduler::SetRoleForRootVmar("fuchsia.ui.scenic");
  if (vmar_status != ZX_OK) {
    FX_LOGS(WARNING) << "Failed to apply profile to root VMAR: " << vmar_status;
  }

  loop.Run();
  FX_LOGS(INFO) << "Quit main Scenic loop.";

  return 0;
}
