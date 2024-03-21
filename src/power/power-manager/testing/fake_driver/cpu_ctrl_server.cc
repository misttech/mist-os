// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "cpu_ctrl_server.h"

#include <lib/driver/logging/cpp/structured_logger.h>

namespace fake_driver {
CpuCtrlProtocolServer::CpuCtrlProtocolServer() {}

void CpuCtrlProtocolServer::GetOperatingPointInfo(GetOperatingPointInfoRequestView request,
                                                  GetOperatingPointInfoCompleter::Sync& completer) {
  if (request->opp >= operating_points_.size()) {
    completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
    return;
  }

  fuchsia_hardware_cpu_ctrl::wire::CpuOperatingPointInfo result;
  result.frequency_hz = operating_points_[request->opp].freq_hz;
  result.voltage_uv = operating_points_[request->opp].volt_uv;

  completer.ReplySuccess(result);
}

void CpuCtrlProtocolServer::SetCurrentOperatingPoint(
    SetCurrentOperatingPointRequestView request,
    SetCurrentOperatingPointCompleter::Sync& completer) {
  std::scoped_lock lock(lock_);
  current_opp_ = request->requested_opp;
  completer.ReplySuccess(current_opp_);
}

void CpuCtrlProtocolServer::GetCurrentOperatingPoint(
    GetCurrentOperatingPointCompleter::Sync& completer) {
  std::scoped_lock lock(lock_);
  completer.Reply(current_opp_);
}

void CpuCtrlProtocolServer::GetOperatingPointCount(
    GetOperatingPointCountCompleter::Sync& completer) {
  completer.ReplySuccess(static_cast<uint32_t>(operating_points_.size()));
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
