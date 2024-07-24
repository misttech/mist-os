// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/bin/system_monitor/system_monitor.h"

#include <fuchsia/diagnostics/cpp/fidl.h>
#include <fuchsia/logger/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/sys/cpp/service_directory.h>
#include <lib/syslog/cpp/macros.h>

#include <algorithm>
#include <iostream>
#include <iterator>
#include <string>

#include <src/lib/diagnostics/accessor2logger/log_message.h>

#include "src/lib/fsl/vmo/strings.h"

namespace system_monitor {

constexpr char kTarget[] = "platform_metrics";
constexpr zx::duration kPrintFrequency = zx::sec(10);

SystemMonitor::SystemMonitor() {
  auto services = sys::ServiceDirectory::CreateFromNamespace();
  services->Connect(accessor_.NewRequest());

  params_ = fuchsia::diagnostics::StreamParameters();
  params_.set_stream_mode(fuchsia::diagnostics::StreamMode::SNAPSHOT);
  params_.set_data_type(fuchsia::diagnostics::DataType::INSPECT);
  params_.set_format(fuchsia::diagnostics::Format::JSON);
  params_.set_client_selector_configuration(
      fuchsia::diagnostics::ClientSelectorConfiguration::WithSelectAll(true));
}

void SystemMonitor::UpdateRecentDiagnostic() {
  recent_diagnostics_.clear();
  accessor_->StreamDiagnostics(std::move(params_), iterator_.NewRequest());
  fuchsia::diagnostics::BatchIterator_GetNext_Result iterator_result;
  iterator_->GetNext(&iterator_result);
  for (auto& content : iterator_result.response().batch) {
    if (!content.is_json()) {
      FX_LOGS(WARNING) << "Invalid JSON Inspect content, skipping";
      continue;
    }
    if (std::string json; fsl::StringFromVmo(content.json(), &json)) {
      recent_diagnostics_.push_back(std::move(json));
    } else {
      FX_LOGS(WARNING) << "Failed to convert Inspect content to string, skipping";
    }
  }
}

void SystemMonitor::PrintRecentDiagnostic() {
  UpdateRecentDiagnostic();
  for (auto& content : recent_diagnostics_) {
    std::size_t found = content.find(kTarget);
    if (found != std::string::npos) {
      FX_LOGS(INFO) << "Recent Diagnostics: " << content;
    }
  }
  async::PostDelayedTask(
      async_get_default_dispatcher(), [&] { PrintRecentDiagnostic(); }, kPrintFrequency);
}
}  // namespace system_monitor
