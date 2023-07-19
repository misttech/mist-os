// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_PROCESS_EXPLORER_PROCESS_EXPLORER_H_
#define SRC_DEVELOPER_PROCESS_EXPLORER_PROCESS_EXPLORER_H_

#include <fidl/fuchsia.process.explorer/cpp/fidl.h>
#include <fuchsia/process/explorer/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/cpp/component_context.h>

#include "src/lib/fxl/command_line.h"

namespace process_explorer {

class Explorer : public fuchsia::process::explorer::Query,
                 public fidl::Server<fuchsia_process_explorer::ProcessExplorer> {
 public:
  Explorer(std::unique_ptr<sys::ComponentContext> context);
  ~Explorer() override;

  // Writes processes information to |socket| in JSON, in UTF-8.
  // See /src/developer/process_explorer/writer.h for a description of the format of the JSON.
  void WriteJsonProcessesData(zx::socket socket) override;

  // fuchsia.process.exploxer/ProcessExplorer implementation.
  void GetTaskInfo(GetTaskInfoRequest& request, GetTaskInfoCompleter::Sync& completer) override;
  void GetHandleInfo(GetHandleInfoRequest& request,
                     GetHandleInfoCompleter::Sync& completer) override;
  void GetVmaps(GetVmapsRequest& request, GetVmapsCompleter::Sync& completer) override;
  void GetStackTrace(GetStackTraceRequest& request,
                     GetStackTraceCompleter::Sync& completer) override;
  void KillTask(KillTaskRequest& request, KillTaskCompleter::Sync& completer) override;

 private:
  std::unique_ptr<sys::ComponentContext> component_context_;
  fidl::BindingSet<fuchsia::process::explorer::Query> bindings_;
};

}  // namespace process_explorer

#endif  // SRC_DEVELOPER_PROCESS_EXPLORER_PROCESS_EXPLORER_H_
