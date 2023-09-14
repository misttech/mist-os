// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/lib/perfmon/controller_impl.h"

#include <fidl/fuchsia.perfmon.cpu/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>

#include "src/performance/lib/perfmon/config_impl.h"
#include "src/performance/lib/perfmon/device_reader.h"

namespace perfmon::internal {

ControllerImpl::ControllerImpl(fidl::SyncClient<fuchsia_perfmon_cpu::Controller> controller_ptr,
                               uint32_t num_traces, uint32_t buffer_size_in_pages,
                               const Config& config)
    : controller_ptr_(std::move(controller_ptr)),
      num_traces_(num_traces),
      buffer_size_in_pages_(buffer_size_in_pages),
      config_(config),
      weak_ptr_factory_(this) {}

ControllerImpl::~ControllerImpl() { Reset(); }

bool ControllerImpl::Start() {
  if (started_) {
    FX_LOGS(ERROR) << "already started";
    return false;
  }

  if (!Stage()) {
    return false;
  }

  if (auto result = controller_ptr_->Start(); result.is_error()) {
    FX_LOGS(ERROR) << "Starting trace failed: status=" << result.error_value();
    return false;
  }

  started_ = true;
  return true;
}

void ControllerImpl::Stop() {
  if (auto result = controller_ptr_->Stop(); result.is_error()) {
    FX_LOGS(ERROR) << "Stopping trace failed: status=" << result.error_value();
  } else {
    started_ = false;
  }
}

bool ControllerImpl::Stage() {
  FX_DCHECK(!started_);

  fuchsia_perfmon_cpu::Config fidl_config;
  internal::PerfmonToFidlConfig(config_, &fidl_config);

  if (auto result = controller_ptr_->StageConfig(fidl_config); result.is_error()) {
    FX_LOGS(ERROR) << "Staging config failed: status=" << result.error_value();
    return false;
  }

  return true;
}

void ControllerImpl::Terminate() {
  if (auto status = controller_ptr_->Terminate(); status.is_error()) {
    FX_LOGS(ERROR) << "Terminating trace failed: status=" << status.error_value();
  } else {
    started_ = false;
  }
}

void ControllerImpl::Reset() {
  Stop();
  Terminate();
}

bool ControllerImpl::GetBufferHandle(const std::string& name, uint32_t trace_num,
                                     zx::vmo* out_vmo) {
  auto out_vmo_res = controller_ptr_->GetBufferHandle(trace_num);
  if (out_vmo_res.is_error()) {
    FX_LOGS(ERROR) << "Getting buffer handle failed: status=" << out_vmo_res.error_value();
    return false;
  }
  if (!out_vmo_res->vmo().is_valid()) {
    FX_LOGS(ERROR) << "Getting buffer handle failed: no handle returned";
    return false;
  }
  out_vmo->reset(out_vmo_res->vmo().release());
  return true;
}

std::unique_ptr<Reader> ControllerImpl::GetReader() {
  std::unique_ptr<Reader> reader;
  if (DeviceReader::Create(weak_ptr_factory_.GetWeakPtr(), buffer_size_in_pages_, &reader)) {
    return reader;
  }
  return nullptr;
}

}  // namespace perfmon::internal
