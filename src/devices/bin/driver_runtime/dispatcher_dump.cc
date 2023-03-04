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

  out_state->running_dispatcher = driver_context::GetCurrentDispatcher();
  out_state->running_driver = driver_context::GetCurrentDriver();
  out_state->dispatcher_to_dump = this;
  out_state->driver_owner =
      out_state->dispatcher_to_dump ? out_state->dispatcher_to_dump->owner() : nullptr;
  out_state->name = name_.ToString();
  out_state->synchronized = !unsynchronized_;
  out_state->allow_sync_calls = allow_sync_calls_;
  out_state->state = state_;
  out_state->queued_tasks.clear();

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
