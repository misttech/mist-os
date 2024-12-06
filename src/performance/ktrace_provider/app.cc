// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/ktrace_provider/app.h"

#include <fidl/fuchsia.tracing.kernel/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fxt/fields.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-engine/instrumentation.h>
#include <lib/trace-provider/provider.h>
#include <lib/zx/channel.h>
#include <unistd.h>
#include <zircon/status.h>
#include <zircon/syscalls/log.h>

#include <iterator>

#include "lib/fit/defer.h"
#include "src/performance/ktrace_provider/device_reader.h"

namespace ktrace_provider {
namespace {

struct KTraceCategory {
  const char* name;
  uint32_t group;
};

constexpr KTraceCategory kGroupCategories[] = {
    {"kernel", KTRACE_GRP_ALL},
    {"kernel:meta", KTRACE_GRP_META},
    {"kernel:lifecycle", KTRACE_GRP_LIFECYCLE},
    {"kernel:sched", KTRACE_GRP_SCHEDULER},
    {"kernel:tasks", KTRACE_GRP_TASKS},
    {"kernel:ipc", KTRACE_GRP_IPC},
    {"kernel:irq", KTRACE_GRP_IRQ},
    {"kernel:probe", KTRACE_GRP_PROBE},
    {"kernel:arch", KTRACE_GRP_ARCH},
    {"kernel:syscall", KTRACE_GRP_SYSCALL},
    {"kernel:vm", KTRACE_GRP_VM},
    {"kernel:restricted", KTRACE_GRP_RESTRICTED},
};

// Meta category to retain current contents of ktrace buffer.
constexpr char kRetainCategory[] = "kernel:retain";

constexpr char kLogCategory[] = "log";

template <typename T>
void LogFidlFailure(const char* rqst_name, const fidl::Result<T>& result) {
  if (result.is_error()) {
    FX_LOGS(ERROR) << "Ktrace FIDL " << rqst_name
                   << " failed: " << result.error_value().status_string();
  } else if (result->status() != ZX_OK) {
    FX_PLOGS(ERROR, result->status()) << "Ktrace " << rqst_name << " failed";
  }
}

void RequestKtraceStop(const fidl::SyncClient<fuchsia_tracing_kernel::Controller>& controller) {
  fidl::Result result = controller->Stop();
  LogFidlFailure("stop", result);
}

void RequestKtraceRewind(const fidl::SyncClient<fuchsia_tracing_kernel::Controller>& controller) {
  fidl::Result result = controller->Rewind();
  LogFidlFailure("rewind", result);
}

void RequestKtraceStart(const fidl::SyncClient<fuchsia_tracing_kernel::Controller>& controller,
                        trace_buffering_mode_t buffering_mode, uint32_t group_mask) {
  using BufferingMode = fuchsia_tracing::BufferingMode;

  BufferingMode fidl_buffering_mode = BufferingMode::kOneshot;
  switch (buffering_mode) {
    // ktrace does not currently support streaming, so for now we preserve the
    // legacy behavior of falling back on one-shot mode.
    case TRACE_BUFFERING_MODE_STREAMING:
    case TRACE_BUFFERING_MODE_ONESHOT:
      fidl_buffering_mode = BufferingMode::kOneshot;
      break;

    case TRACE_BUFFERING_MODE_CIRCULAR:
      fidl_buffering_mode = BufferingMode::kCircular;
      break;

    default:
      FX_PLOGS(ERROR, ZX_ERR_INVALID_ARGS) << "Invalid buffering mode: " << buffering_mode;
      return;
  }

  fidl::Result status =
      controller->Start({{.group_mask = group_mask, .buffering_mode = fidl_buffering_mode}});

  LogFidlFailure("start", status);
}

}  // namespace

std::vector<trace::KnownCategory> GetKnownCategories() {
  std::vector<trace::KnownCategory> known_categories = {
      {.name = kRetainCategory,
       .description = "Retain the previous contents of the buffer instead of clearing it out"},
  };

  for (const auto& category : kGroupCategories) {
    known_categories.emplace_back(category.name, "");
  }

  return known_categories;
}

App::App(const fxl::CommandLine& command_line) {
  trace_observer_.Start(async_get_default_dispatcher(), [this] { UpdateState(); });
}

App::~App() = default;

void App::UpdateState() {
  uint32_t group_mask = 0;
  bool capture_log = false;
  bool retain_current_data = false;
  if (trace_state() == TRACE_STARTED) {
    size_t num_enabled_categories = 0;
    for (const auto& category : kGroupCategories) {
      if (trace_is_category_enabled(category.name)) {
        group_mask |= category.group;
        ++num_enabled_categories;
      }
    }

    // Avoid capturing log traces in the default case by detecting whether all
    // categories are enabled or not.
    capture_log = trace_is_category_enabled(kLogCategory) &&
                  num_enabled_categories != std::size(kGroupCategories);

    // The default case is everything is enabled, but |kRetainCategory| must be
    // explicitly passed.
    retain_current_data = trace_is_category_enabled(kRetainCategory) &&
                          num_enabled_categories != std::size(kGroupCategories);
  }

  if (current_group_mask_ != group_mask) {
    trace_context_t* ctx = trace_acquire_context();

    StopKTrace();
    StartKTrace(group_mask, trace_context_get_buffering_mode(ctx), retain_current_data);

    if (ctx != nullptr) {
      trace_release_context(ctx);
    }
  }

  if (capture_log) {
    log_importer_.Start();
  } else {
    log_importer_.Stop();
  }
}

void App::StartKTrace(uint32_t group_mask, trace_buffering_mode_t buffering_mode,
                      bool retain_current_data) {
  FX_DCHECK(!context_);
  if (!group_mask) {
    return;  // nothing to trace
  }

  FX_LOGS(INFO) << "Starting ktrace";

  zx::result client_end = component::Connect<fuchsia_tracing_kernel::Controller>();
  if (client_end.is_error()) {
    FX_PLOGS(ERROR, client_end.error_value()) << " failed to connect to ktrace controller";
    return;
  }
  auto ktrace_controller = fidl::SyncClient{std::move(*client_end)};

  context_ = trace_acquire_prolonged_context();
  if (!context_) {
    // Tracing was disabled in the meantime.
    return;
  }
  current_group_mask_ = group_mask;

  RequestKtraceStop(ktrace_controller);
  if (!retain_current_data) {
    RequestKtraceRewind(ktrace_controller);
  }
  RequestKtraceStart(ktrace_controller, buffering_mode, group_mask);

  FX_LOGS(DEBUG) << "Ktrace started";
}

void DrainBuffer(std::unique_ptr<DrainContext> drain_context) {
  if (!drain_context) {
    return;
  }

  trace_context_t* buffer_context = trace_acquire_context();
  auto d = fit::defer([buffer_context]() { trace_release_context(buffer_context); });
  for (std::optional<uint64_t> fxt_header = drain_context->reader.PeekNextHeader();
       fxt_header.has_value(); fxt_header = drain_context->reader.PeekNextHeader()) {
    size_t record_size_bytes = fxt::RecordFields::RecordSize::Get<size_t>(*fxt_header) * 8;
    // We try to be a bit too clever here and check that there is enough space before writing a
    // record to the buffer. If we're in streaming mode, and there isn't space for the record, this
    // will show up as a dropped record even though we retry later. Unfortunately, there isn't
    // currently a good api exposed.
    //
    // TODO(issues.fuchsia.dev/304532640): Investigate a method to allow trace providers to wait on
    // a full buffer
    if (void* dst = trace_context_alloc_record(buffer_context, record_size_bytes); dst != nullptr) {
      const uint64_t* record = drain_context->reader.ReadNextRecord();
      memcpy(dst, reinterpret_cast<const char*>(record), record_size_bytes);
    } else {
      if (trace_context_get_buffering_mode(buffer_context) == TRACE_BUFFERING_MODE_STREAMING) {
        // We are writing out our data on the async loop. Notifying the trace manager to begin
        // saving the data also requires the context and occurs on the loop. If we run out of space,
        // we'll release the loop and reschedule ourself to allow the buffer saving to begin.
        //
        // We are memcpy'ing data here and trace_manager is writing the buffer to a socket (likely
        // shared with ffx), the cost to copy the kernel buffer to the trace buffer here pales in
        // comparison to the cost of what trace_manager is doing. We'll poll here with a slight
        // delay until the buffer is ready.
        async::PostDelayedTask(
            async_get_default_dispatcher(),
            [drain_context = std::move(drain_context)]() mutable {
              DrainBuffer(std::move(drain_context));
            },
            zx::msec(100));
        return;
      }
      // Outside of streaming mode, we aren't going to get more space. We'll need to read in this
      // record and just drop it. Rather than immediately exiting, we allow the loop to continue so
      // that we correctly enumerate all the dropped records for statistical reporting.
      drain_context->reader.ReadNextRecord();
    }
  }

  // Done writing trace data
  size_t bytes_read = drain_context->reader.number_bytes_read();
  zx::duration time_taken = zx::clock::get_monotonic() - drain_context->start;
  double bytes_per_sec = static_cast<double>(bytes_read) /
                         static_cast<double>(std::max(int64_t{1}, time_taken.to_usecs()));
  FX_LOGS(INFO) << "Import of " << drain_context->reader.number_records_read() << " kernel records"
                << "(" << bytes_read << " bytes) took: " << time_taken.to_msecs()
                << "ms. MBytes/sec: " << bytes_per_sec;
  FX_LOGS(DEBUG) << "Ktrace stopped";
}

void App::StopKTrace() {
  if (!context_) {
    return;  // not currently tracing
  }
  auto d = fit::defer([this]() {
    trace_release_prolonged_context(context_);
    context_ = nullptr;
    current_group_mask_ = 0u;
  });
  FX_DCHECK(current_group_mask_);

  FX_LOGS(INFO) << "Stopping ktrace";

  {
    zx::result client_end = component::Connect<fuchsia_tracing_kernel::Controller>();
    if (client_end.is_error()) {
      FX_PLOGS(ERROR, client_end.error_value()) << " failed to connect to ktrace controller";
      return;
    }
    auto ktrace_controller = fidl::SyncClient{std::move(*client_end)};
    RequestKtraceStop(ktrace_controller);
  }

  auto drain_context = DrainContext::Create();
  if (!drain_context) {
    FX_LOGS(ERROR) << "Failed to start reading kernel buffer";
    return;
  }
  zx_status_t result = async::PostTask(async_get_default_dispatcher(),
                                       [drain_context = std::move(drain_context)]() mutable {
                                         DrainBuffer(std::move(drain_context));
                                       });
  if (result != ZX_OK) {
    FX_PLOGS(ERROR, result) << "Failed to schedule buffer writer";
  }
}

}  // namespace ktrace_provider
