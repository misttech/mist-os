// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "cpu_ctrl_server.h"

#include <lib/driver/logging/cpp/structured_logger.h>

namespace fake_driver {
CpuCtrlProtocolServer::CpuCtrlProtocolServer() {}

void CpuCtrlProtocolServer::GetPerformanceStateInfo(
    GetPerformanceStateInfoRequestView request, GetPerformanceStateInfoCompleter::Sync& completer) {
  if (request->state >= operating_points_.size()) {
    completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
    return;
  }

  fuchsia_hardware_cpu_ctrl::wire::CpuPerformanceStateInfo result;
  result.frequency_hz = operating_points_[request->state].freq_hz;
  result.voltage_uv = operating_points_[request->state].volt_uv;

  completer.ReplySuccess(result);
}

void CpuCtrlProtocolServer::SetPerformanceState(SetPerformanceStateRequestView request,
                                                SetPerformanceStateCompleter::Sync& completer) {
  std::scoped_lock lock(lock_);
  current_pstate_ = request->requested_state;
  completer.ReplySuccess(current_pstate_);
}

void CpuCtrlProtocolServer::GetCurrentPerformanceState(
    GetCurrentPerformanceStateCompleter::Sync& completer) {
  std::scoped_lock lock(lock_);
  completer.Reply(current_pstate_);
}

void CpuCtrlProtocolServer::GetNumLogicalCores(GetNumLogicalCoresCompleter::Sync& completer) {
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void CpuCtrlProtocolServer::GetLogicalCoreId(GetLogicalCoreIdRequestView request,
                                             GetLogicalCoreIdCompleter::Sync& completer) {
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void CpuCtrlProtocolServer::Serve(async_dispatcher_t* dispatcher,
                                  fidl::ServerEnd<fuchsia_hardware_cpu_ctrl::Device> server) {
  bindings_.AddBinding(dispatcher, std::move(server), this, fidl::kIgnoreBindingClosure);
}

}  // namespace fake_driver
