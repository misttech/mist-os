// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_KTRACE_PROVIDER_APP_H_
#define SRC_PERFORMANCE_KTRACE_PROVIDER_APP_H_

#include <lib/sys/cpp/component_context.h>
#include <lib/sys/cpp/service_directory.h>
#include <lib/trace-provider/provider.h>
#include <lib/trace/observer.h>

#include <fbl/unique_fd.h>

#include "src/lib/fxl/command_line.h"
#include "src/performance/ktrace_provider/device_reader.h"
#include "src/performance/ktrace_provider/log_importer.h"

namespace ktrace_provider {

std::vector<trace::KnownCategory> GetKnownCategories();

struct DrainContext {
  DrainContext(zx::time start, trace_prolonged_context_t* context)
      : start(start), context(context) {}

  static std::unique_ptr<DrainContext> Create() {
    auto context = trace_acquire_prolonged_context();
    if (context == nullptr) {
      return nullptr;
    }
    auto out = std::make_unique<DrainContext>(zx::clock::get_monotonic(), context);
    if (zx_status_t result = out->reader.Init(); result != ZX_OK) {
      return nullptr;
    }

    return out;
  }

  ~DrainContext() { trace_release_prolonged_context(context); }
  zx::time start;
  DeviceReader reader;
  trace_prolonged_context_t* context;
};

class App {
 public:
  explicit App(zx::resource debug_resource, const fxl::CommandLine& command_line);
  ~App();

 private:
  zx::result<> UpdateState();

  zx::result<> StartKTrace(uint32_t group_mask, trace_buffering_mode_t buffering_mode,
                           bool retain_current_data);
  zx::result<> StopKTrace();

  trace::TraceObserver trace_observer_;
  LogImporter log_importer_;
  uint32_t current_group_mask_ = 0u;
  // This context keeps the trace context alive until we've written our trace
  // records, which doesn't happen until after tracing has stopped.
  trace_prolonged_context_t* context_ = nullptr;
  zx::resource debug_resource_;

  App(const App&) = delete;
  App(App&&) = delete;
  App& operator=(const App&) = delete;
  App& operator=(App&&) = delete;
};

}  // namespace ktrace_provider

#endif  // SRC_PERFORMANCE_KTRACE_PROVIDER_APP_H_
