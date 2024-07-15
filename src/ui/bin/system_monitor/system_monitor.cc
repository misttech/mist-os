// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/bin/system_monitor/system_monitor.h"

#include <fuchsia/diagnostics/cpp/fidl.h>
#include <fuchsia/logger/cpp/fidl.h>
#include <lib/sys/cpp/service_directory.h>
#include <lib/syslog/cpp/macros.h>

#include <algorithm>
#include <iostream>
#include <iterator>
#include <string>

#include <src/lib/diagnostics/accessor2logger/log_message.h>

#include "src/lib/fsl/vmo/strings.h"

namespace system_monitor {

SystemMonitor::SystemMonitor() {
  fuchsia::diagnostics::ArchiveAccessorSyncPtr accessor;
  auto services = sys::ServiceDirectory::CreateFromNamespace();
  services->Connect(accessor.NewRequest());

  auto params = fuchsia::diagnostics::StreamParameters();
  params.set_stream_mode(fuchsia::diagnostics::StreamMode::SNAPSHOT);
  params.set_data_type(fuchsia::diagnostics::DataType::INSPECT);
  params.set_format(fuchsia::diagnostics::Format::JSON);
  params.set_client_selector_configuration(
      fuchsia::diagnostics::ClientSelectorConfiguration::WithSelectAll(true));

  accessor->StreamDiagnostics(std::move(params), iterator.NewRequest());
}

std::vector<std::string> SystemMonitor::updateRecentDiagnostic() {
  fuchsia::diagnostics::BatchIterator_GetNext_Result iteratorResult;
  iterator->GetNext(&iteratorResult);
  for (auto& content : iteratorResult.response().batch) {
    if (!content.is_json()) {
      FX_LOGS(WARNING) << "Invalid JSON Inspect content, skipping";
      continue;
    }

    if (std::string json; fsl::StringFromVmo(content.json(), &json)) {
      recentDiagnostic.push_back(std::move(json));
    } else {
      FX_LOGS(WARNING) << "Failed to convert Inspect content to string, skipping";
    }
  }
  return recentDiagnostic;
}
}  // namespace system_monitor
