// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/bin/system_monitor/system_monitor.h"

#include <fuchsia/diagnostics/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/sys/cpp/service_directory.h>
#include <lib/syslog/cpp/macros.h>

#include <algorithm>
#include <iostream>
#include <iterator>
#include <string>

#include "src/lib/fsl/vmo/strings.h"

namespace system_monitor {

constexpr char kTarget[] = "platform_metrics";
constexpr zx::duration kPrintFrequency = zx::sec(10);

SystemMonitor::SystemMonitor() {
  params_ = fuchsia::diagnostics::StreamParameters();
  params_.set_stream_mode(fuchsia::diagnostics::StreamMode::SNAPSHOT);
  params_.set_data_type(fuchsia::diagnostics::DataType::INSPECT);
  params_.set_format(fuchsia::diagnostics::Format::JSON);
  params_.set_client_selector_configuration(
      fuchsia::diagnostics::ClientSelectorConfiguration::WithSelectAll(true));
}

void SystemMonitor::ConnectToArchiveAccessor(bool use_real_archive_accessor) {
  auto services = sys::ServiceDirectory::CreateFromNamespace();
  if (use_real_archive_accessor) {
    services->Connect(accessor_.NewRequest(), "fuchsia.diagnostics.RealArchiveAccessor");
  } else {
    services->Connect(accessor_.NewRequest());
  }
}

void SystemMonitor::InitializeRenderer() { renderer_.Initialize(); }

void SystemMonitor::UpdateRecentDiagnostic() {
  accessor_->StreamDiagnostics(std::move(params_), iterator_.NewRequest());
  iterator_->GetNext(&iterator_result_);
}

std::vector<std::string> SystemMonitor::ParseBatch(
    const std::vector<fuchsia::diagnostics::FormattedContent>& batch) {
  std::vector<std::string> recent_diagnostics;
  for (auto& content : batch) {
    if (!content.is_json()) {
      FX_LOGS(WARNING) << "Invalid JSON Inspect content, skipping";
      continue;
    }
    if (std::string json; fsl::StringFromVmo(content.json(), &json)) {
      recent_diagnostics.push_back(std::move(json));
    } else {
      FX_LOGS(WARNING) << "Failed to convert Inspect content to string, skipping";
    }
  }
  return recent_diagnostics;
}

std::string SystemMonitor::GetTargetFromDiagnostics(std::vector<std::string> recent_diagnostics) {
  std::string cpu_data;
  for (auto& content : recent_diagnostics) {
    if (content.find(kTarget) != std::string::npos) {
      cpu_data = content;
    }
  }
  return cpu_data;
}

std::string SystemMonitor::GetCPUData() {
  auto& batch = iterator_result_.response().batch;
  if (batch.empty()) {
    return "";
  }
  return GetTargetFromDiagnostics(ParseBatch(std::move(batch)));
}

void SystemMonitor::PrintRecentDiagnostic() {
  UpdateRecentDiagnostic();
  FX_LOGS(INFO) << "Recent Diagnostics: " << GetCPUData();

  async::PostDelayedTask(
      async_get_default_dispatcher(), [&] { PrintRecentDiagnostic(); }, kPrintFrequency);
}
}  // namespace system_monitor
