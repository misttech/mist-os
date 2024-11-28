// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_runtime/dispatcher.h"

namespace driver_runtime {

namespace {

const char* DispatcherStateToString(Dispatcher::DispatcherState state) {
  switch (state) {
    case Dispatcher::DispatcherState::kRunning:
      return "running";
    case Dispatcher::DispatcherState::kShuttingDown:
      return "shutting down";
    case Dispatcher::DispatcherState::kShutdown:
      return "shutdown";
    case Dispatcher::DispatcherState::kDestroyed:
      return "destroyed";
  }
  return "unknown dispatcher state";
}

const char* BoolToString(bool b) { return b ? "true" : "false"; }

void OutputFormattedString(std::vector<std::string>* dump_out, const char* fmt, ...) {
  va_list args;
  va_start(args, fmt);
  int size = std::vsnprintf(nullptr, 0, fmt, args);
  std::vector<char> buf(size + 1);
  std::vsnprintf(buf.data(), buf.size(), fmt, args);
  dump_out->push_back(&buf[0]);
  va_end(args);
}

void AppendCallbackRequestAsTask(Dispatcher::DumpState* out_state,
                                 CallbackRequest& callback_request) {
  ZX_ASSERT(callback_request.request_type() == CallbackRequest::RequestType::kTask);
  async_task_t* task = static_cast<async_task_t*>(callback_request.async_operation());
  out_state->queued_tasks.push_back(Dispatcher::TaskDebugInfo{
      .ptr = task,
      .handler = task->handler,
      .initiating_dispatcher = callback_request.initiating_dispatcher(),
      .initiating_driver = callback_request.initiating_driver(),
  });
}

}  // namespace

void Dispatcher::DumpToString(std::vector<std::string>* dump_out) {
  DumpState dump_state;
  Dump(&dump_state);
  FormatDump(&dump_state, dump_out);
}

void Dispatcher::Dump(DumpState* out_state) {
  fbl::AutoLock lock(&callback_lock_);

  out_state->running_dispatcher = thread_context::GetCurrentDispatcher();
  out_state->running_driver = thread_context::GetCurrentDriver();
  out_state->dispatcher_to_dump = this;
  out_state->driver_owner =
      out_state->dispatcher_to_dump ? out_state->dispatcher_to_dump->owner() : nullptr;
  out_state->name = name_.ToString();
  out_state->synchronized = !unsynchronized_;
  out_state->allow_sync_calls = allow_sync_calls_;
  out_state->state = state_;
  out_state->queued_tasks.clear();
  out_state->debug_stats = debug_stats_;

  for (auto& callback_request : callback_queue_) {
    if (callback_request.request_type() == CallbackRequest::RequestType::kTask) {
      AppendCallbackRequestAsTask(out_state, callback_request);
    }
  }
  for (auto& callback_request : shutdown_queue_) {
    if (callback_request.request_type() == CallbackRequest::RequestType::kTask) {
      AppendCallbackRequestAsTask(out_state, callback_request);
    }
  }
}

void Dispatcher::FormatDump(DumpState* state, std::vector<std::string>* dump_out) {
  dump_out->clear();

  OutputFormattedString(dump_out, "---- Runtime Dispatcher Dump Begin ----");
  if (state->running_dispatcher) {
    OutputFormattedString(
        dump_out,
        "The current thread is managed by the driver runtime, running dispatcher: %p (driver %p)",
        state->running_dispatcher, state->running_driver);
  } else {
    OutputFormattedString(dump_out, "The current thread is not managed by the driver runtime.");
  }
  OutputFormattedString(dump_out, "Dispatcher to dump: %s (%p) (driver %p)", state->name.data(),
                        state->dispatcher_to_dump, state->driver_owner);
  OutputFormattedString(dump_out, "Synchronized: %s", BoolToString(state->synchronized));
  OutputFormattedString(dump_out, "Allow sync calls: %s", BoolToString(state->allow_sync_calls));
  OutputFormattedString(dump_out, "State: %s", DispatcherStateToString(state->state));
  OutputFormattedString(dump_out, "Processed %lu requests, %lu were inlined",
                        state->debug_stats.num_total_requests,
                        state->debug_stats.num_inlined_requests);

  const auto& non_inlined_stats = state->debug_stats.non_inlined;
  if (state->debug_stats.num_total_requests != state->debug_stats.num_inlined_requests) {
    OutputFormattedString(dump_out, "Reasons why requests were not inlined:");
    if (non_inlined_stats.allow_sync_calls > 0) {
      OutputFormattedString(
          dump_out,
          "* Calling from a non-blocking to a blocking (ALLOW_SYNC_CALLS) dispatcher: %lu times",
          non_inlined_stats.allow_sync_calls);
    }
    if (non_inlined_stats.parallel_dispatch > 0) {
      OutputFormattedString(dump_out,
                            "* Another thread was already dispatching a request: %lu times",
                            non_inlined_stats.parallel_dispatch);
    }
    if (non_inlined_stats.task > 0) {
      OutputFormattedString(dump_out, "* The request was a task: %lu times",
                            non_inlined_stats.task);
    }
    if (non_inlined_stats.unknown_thread > 0) {
      OutputFormattedString(dump_out, "* The request was queued from an unknown thread: %lu times",
                            non_inlined_stats.unknown_thread);
    }
    if (non_inlined_stats.reentrant > 0) {
      OutputFormattedString(dump_out, "* The request would have been reentrant: %lu times",
                            non_inlined_stats.reentrant);
    }
    if (non_inlined_stats.channel_wait_not_yet_registered > 0) {
      OutputFormattedString(
          dump_out,
          "* Channel wait was not yet registered when the message was received: %lu times",
          non_inlined_stats.channel_wait_not_yet_registered);
    }
  }

  if (state->queued_tasks.empty()) {
    OutputFormattedString(dump_out, "No queued tasks");
  } else {
    OutputFormattedString(dump_out, "%lu Queued Tasks:", state->queued_tasks.size());
    for (auto task : state->queued_tasks) {
      OutputFormattedString(dump_out, "Task: %p Handler: %p", task.ptr, task.handler);
      if (task.initiating_dispatcher) {
        OutputFormattedString(dump_out, " - Task was queued by dispatcher %p (driver %p)",
                              task.initiating_dispatcher, task.initiating_driver);
      } else {
        OutputFormattedString(dump_out, " - Task was not queued from a runtime thread");
      }
    }
  }
}

}  // namespace driver_runtime
