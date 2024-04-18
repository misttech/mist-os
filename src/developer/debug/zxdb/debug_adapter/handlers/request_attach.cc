// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/debug_adapter/handlers/request_attach.h"

#include "src/developer/debug/ipc/records.h"
#include "src/developer/debug/zxdb/client/filter.h"
#include "src/developer/debug/zxdb/client/session.h"
#include "src/developer/debug/zxdb/debug_adapter/handlers/request_launch.h"

namespace dap {

DAP_IMPLEMENT_STRUCT_TYPEINFO_EXT(AttachRequestZxdb, AttachRequest, "attach",
                                  DAP_FIELD(process, "process"), DAP_FIELD(command, "command"),
                                  DAP_FIELD(recursive, "recursive"), DAP_FIELD(cwd, "cwd"))

}  // namespace dap

namespace zxdb {

dap::ResponseOrError<dap::AttachResponse> OnRequestAttach(DebugAdapterContext* context,
                                                          const dap::AttachRequestZxdb& req) {
  dap::AttachResponse response;
  std::string attach = req.recursive && *req.recursive ? "attach --recursive " : "attach ";
  context->console()->ProcessInputLine(attach + req.process);

  // If specified run the provided command (an event) every time we start debugging
  // or after clicking the restart button.
  if (req.command) {
    dap::RunInTerminalRequest run_request;
    run_request.title = "zxdb launch";
    run_request.kind = "integrated";
    SplitDapCommand(req.command.value(), run_request.args);
    if (req.cwd) {
      run_request.cwd = req.cwd.value();
    }
    context->dap().send(run_request);
  }

  return response;
}

}  // namespace zxdb
