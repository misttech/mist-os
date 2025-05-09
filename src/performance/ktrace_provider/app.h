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

#ifdef EXPERIMENTAL_KTRACE_STREAMING_ENABLED
constexpr bool kKernelStreamingSupport = EXPERIMENTAL_KTRACE_STREAMING_ENABLED;
#else
constexpr bool kKernelStreamingSupport = false;
#endif

namespace ktrace_provider {

std::vector<trace::KnownCategory> GetKnownCategories();

struct DrainContext {
  DrainContext(zx::time start, trace_prolonged_context_t* context, zx::resource tracing_resource,
               zx::duration poll_period, std::unique_ptr<uint64_t[]> buffer, size_t buffer_size)
      : start(start),
        reader(std::move(tracing_resource)),
        context(context),
        poll_period(poll_period),
        buffer(std::move(buffer)),
        buffer_size(buffer_size) {}

  // We have a buffer allocated as part of the struct which we don't want to copy or move.
  DrainContext(const DrainContext& other) = delete;
  DrainContext& operator=(const DrainContext& other) = delete;
  DrainContext(DrainContext&& other) = delete;
  DrainContext& operator=(DrainContext&& other) = delete;

  static std::unique_ptr<DrainContext> Create(const zx::resource& tracing_resource,
                                              zx::duration poll_period) {
    trace_prolonged_context_t* context = nullptr;
    if constexpr (!kKernelStreamingSupport) {
      context = trace_acquire_prolonged_context();
      if (context == nullptr) {
        return nullptr;
      }
    }
    zx::resource cloned_resource;
    zx_status_t res = tracing_resource.duplicate(ZX_RIGHT_SAME_RIGHTS, &cloned_resource);
    if (res != ZX_OK) {
      return nullptr;
    }

    std::unique_ptr<uint64_t[]> buffer;
    size_t buffer_size = 0;
    // In streaming mode, we need to ask how big our buffer to copy data into needs to be.
    // In non-streaming mode, this buffer isn't used and we defer to the device reader
    if constexpr (kKernelStreamingSupport) {
      // We ask the kernel using zx_ktrace_read with a nullptr;
      zx_status_t status = zx_ktrace_read(tracing_resource.get(), nullptr, 0, 0, &buffer_size);
      if (status != ZX_OK) {
        return nullptr;
      }
      buffer = std::make_unique<uint64_t[]>(buffer_size / sizeof(uint64_t));
    }

    return std::make_unique<DrainContext>(zx::clock::get_monotonic(), context,
                                          std::move(cloned_resource), poll_period,
                                          std::move(buffer), buffer_size);
  }

  ~DrainContext() {
    if (context != nullptr) {
      trace_release_prolonged_context(context);
    }
  }
  zx::time start;
  DeviceReader reader;
  trace_prolonged_context_t* context;
  zx::duration poll_period;

  // For kernel streaming, we don't use the DeviceReader, we just do all the data management here.
  std::unique_ptr<uint64_t[]> buffer;
  size_t buffer_size;
};

class App {
 public:
  explicit App(zx::resource tracing_resource, const fxl::CommandLine& command_line);
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
  zx::resource tracing_resource_;

  App(const App&) = delete;
  App(App&&) = delete;
  App& operator=(const App&) = delete;
  App& operator=(App&&) = delete;
};

}  // namespace ktrace_provider

#endif  // SRC_PERFORMANCE_KTRACE_PROVIDER_APP_H_
