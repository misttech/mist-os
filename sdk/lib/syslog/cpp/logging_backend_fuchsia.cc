// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <fidl/fuchsia.logger/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/executor.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/stdcompat/variant.h>
#include <lib/sync/completion.h>
#include <lib/syslog/cpp/log_level.h>
#include <lib/syslog/cpp/logging_backend.h>
#include <lib/syslog/cpp/logging_backend_fuchsia_globals.h>
#include <lib/syslog/structured_backend/cpp/fuchsia_syslog.h>
#include <lib/zx/channel.h>
#include <lib/zx/process.h>

#include <cstddef>
#include <fstream>
#include <iostream>
#include <sstream>

#include "lib/syslog/cpp/macros.h"

namespace syslog_runtime {

bool HasStructuredBackend() { return true; }

using log_word_t = uint64_t;

zx_koid_t GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  // We need to use _zx_object_get_info to avoid breaking the driver ABI.
  // fake_ddk can fake out this method, which results in us deadlocking
  // when used in certain drivers because the fake doesn't properly pass-through
  // to the real syscall in this case.
  zx_status_t status =
      _zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

static zx_koid_t pid = GetKoid(zx_process_self());
static thread_local zx_koid_t tid = FuchsiaLogGetCurrentThreadKoid();

struct RecordState {
  // Message string -- valid if severity is FATAL. For FATAL
  // logs the caller is responsible for ensuring the string
  // is valid for the duration of the call (which our macros
  // will ensure for current users). This must be a
  // const char* as the type has to be trivially destructable.
  // This will leak on usage, as the process will crash shortly afterwards.
  const char* maybe_fatal_string;

  fuchsia_syslog::LogBuffer buffer;
  zx_handle_t socket;
  FuchsiaLogSeverity raw_severity;
  static RecordState* CreatePtr(LogBuffer* buffer) {
    return reinterpret_cast<RecordState*>(&buffer->record_state);
  }
};
static_assert(sizeof(RecordState) <= sizeof(LogBuffer::record_state) + sizeof(LogBuffer::data));
static_assert(offsetof(LogBuffer, data) ==
              offsetof(LogBuffer, record_state) + sizeof(LogBuffer::record_state));
static_assert(std::alignment_of<RecordState>() == sizeof(uint64_t));

const size_t kMaxTags = 4;  // Legacy from ulib/syslog. Might be worth rethinking.
const char kTagFieldName[] = "tag";

class GlobalStateLock;
class LogState {
 public:
  static void Set(const fuchsia_logging::LogSettings& settings, const GlobalStateLock& lock);
  static void Set(const fuchsia_logging::LogSettings& settings,
                  const std::initializer_list<std::string>& tags, const GlobalStateLock& lock);
  void set_severity_handler(void (*callback)(void* context, fuchsia_logging::LogSeverity severity),
                            void* context) {
    handler_ = callback;
    handler_context_ = context;
  }

  fuchsia_logging::LogSeverity min_severity() const { return min_severity_; }

  const std::string* tags() const { return tags_; }
  size_t tag_count() const { return num_tags_; }
  // Allowed to be const because descriptor_ is mutable
  cpp17::variant<zx::socket, std::ofstream>& descriptor() const { return descriptor_; }

  void HandleInterest();

  void Connect();
  void ConnectAsync();
  struct Task : public async_task_t {
    LogState* context;
    sync_completion_t completion;
  };
  static void RunTask(async_dispatcher_t* dispatcher, async_task_t* task, zx_status_t status) {
    Task* callback = static_cast<Task*>(task);
    callback->context->ConnectAsync();
    sync_completion_signal(&callback->completion);
  }

  // Thunk to initialize logging and allocate HLCPP objects
  // which are "thread-hostile" and cannot be allocated on a remote thread.
  void InitializeAsyncTask() {
    Task task = {};
    task.deadline = 0;
    task.handler = RunTask;
    task.context = this;
    async_post_task(executor_->dispatcher(), &task);
    sync_completion_wait(&task.completion, ZX_TIME_INFINITE);
  }

 private:
  LogState(const fuchsia_logging::LogSettings& settings,
           const std::initializer_list<std::string>& tags);

  fidl::SharedClient<fuchsia_logger::LogSink> log_sink_;
  void (*handler_)(void* context, fuchsia_logging::LogSeverity severity);
  void* handler_context_;
  async::Loop loop_;
  std::optional<async::Executor> executor_;
  std::atomic<fuchsia_logging::LogSeverity> min_severity_;
  const fuchsia_logging::LogSeverity default_severity_;
  mutable cpp17::variant<zx::socket, std::ofstream> descriptor_ = zx::socket();
  std::string tags_[kMaxTags];
  size_t num_tags_ = 0;
  async_dispatcher_t* interest_listener_dispatcher_;
  bool serve_interest_listener_;
  bool wait_for_initial_interest_;
  // Handle to a fuchsia.logger.LogSink instance.
  zx_handle_t provided_log_sink_ = ZX_HANDLE_INVALID;
};

// Global state lock. In order to mutate the LogState through SetStateLocked
// and GetStateLocked you must hold this capability.
// Do not directly use the C API. The C API exists solely
// to expose globals as a shared library.
// If the logger is not initialized, this will implicitly init the logger.
class GlobalStateLock {
 public:
  GlobalStateLock() {
    FuchsiaLogAcquireState();
    if (!FuchsiaLogGetStateLocked()) {
      LogState::Set(fuchsia_logging::LogSettings(), *this);
    }
  }

  // Retrieves the global state
  syslog_runtime::LogState* operator->() const { return FuchsiaLogGetStateLocked(); }

  // Sets the global state
  void Set(syslog_runtime::LogState* state) const { FuchsiaLogSetStateLocked(state); }

  // Retrieves the global state
  syslog_runtime::LogState* operator*() const { return FuchsiaLogGetStateLocked(); }

  ~GlobalStateLock() { FuchsiaLogReleaseState(); }
};

static fuchsia_logging::LogSeverity IntoLogSeverity(fuchsia_diagnostics::Severity severity) {
  switch (severity) {
    case fuchsia_diagnostics::Severity::kTrace:
      return fuchsia_logging::LOG_TRACE;
      break;
    case fuchsia_diagnostics::Severity::kDebug:
      return fuchsia_logging::LOG_DEBUG;
      break;
    case fuchsia_diagnostics::Severity::kInfo:
      return fuchsia_logging::LOG_INFO;
      break;
    case fuchsia_diagnostics::Severity::kWarn:
      return fuchsia_logging::LOG_WARNING;
      break;
    case fuchsia_diagnostics::Severity::kError:
      return fuchsia_logging::LOG_ERROR;
      break;
    case fuchsia_diagnostics::Severity::kFatal:
      return fuchsia_logging::LOG_FATAL;
      break;
  }
}

void LogState::HandleInterest() {
  log_sink_->WaitForInterestChange().Then(
      [=](fidl::Result<fuchsia_logger::LogSink::WaitForInterestChange>& interest_result) {
        // FIDL can cancel the operation if the logger is being reconfigured
        // which results in an error.
        if (interest_result.is_error()) {
          return;
        }
        auto interest = std::move(interest_result->data());
        if (!interest.min_severity()) {
          min_severity_ = default_severity_;
        } else {
          min_severity_ = IntoLogSeverity(*interest.min_severity());
        }
        handler_(handler_context_, min_severity_);
        HandleInterest();
      });
}

void LogState::ConnectAsync() {
  zx::channel logger, logger_request;
  if (zx::channel::create(0, &logger, &logger_request) != ZX_OK) {
    return;
  }
  if (provided_log_sink_ == ZX_HANDLE_INVALID) {
    // TODO(https://fxbug.dev/42154983): Support for custom names.
    if (fdio_service_connect("/svc/fuchsia.logger.LogSink", logger_request.release()) != ZX_OK) {
      return;
    }
  } else {
    logger = zx::channel(provided_log_sink_);
    provided_log_sink_ = ZX_HANDLE_INVALID;
  }

  if (wait_for_initial_interest_) {
    fidl::SyncClient<fuchsia_logger::LogSink> sync_log_sink;
    sync_log_sink.Bind(fidl::ClientEnd<fuchsia_logger::LogSink>(std::move(logger)));
    auto interest_result = sync_log_sink->WaitForInterestChange();
    auto interest = std::move(interest_result->data());
    if (!interest.min_severity()) {
      min_severity_ = default_severity_;
    } else {
      min_severity_ = IntoLogSeverity(*interest.min_severity());
    }
    handler_(handler_context_, min_severity_);
    logger = sync_log_sink.TakeClientEnd().TakeChannel();
  }

  log_sink_.Bind(fidl::ClientEnd<fuchsia_logger::LogSink>(std::move(logger)), loop_.dispatcher());
  zx::socket local, remote;
  if (zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote) != ZX_OK) {
    return;
  }
  fidl::Request<fuchsia_logger::LogSink::ConnectStructured> request;
  request.socket(std::move(remote));
  if (log_sink_->ConnectStructured(std::move(request)).is_error()) {
    return;
  }

  HandleInterest();
  descriptor_ = std::move(local);
}

void LogState::Connect() {
  if (serve_interest_listener_) {
    if (!interest_listener_dispatcher_) {
      loop_.StartThread("log-interest-listener-thread");
    } else {
      executor_.emplace(interest_listener_dispatcher_);
    }
    handler_ = [](void* ctx, fuchsia_logging::LogSeverity severity) {};
    InitializeAsyncTask();
  } else {
    zx::channel logger, logger_request;
    if (provided_log_sink_ == ZX_HANDLE_INVALID) {
      if (zx::channel::create(0, &logger, &logger_request) != ZX_OK) {
        return;
      }
      // TODO(https://fxbug.dev/42154983): Support for custom names.
      if (fdio_service_connect("/svc/fuchsia.logger.LogSink", logger_request.release()) != ZX_OK) {
        return;
      }
    } else {
      logger = zx::channel(provided_log_sink_);
    }
    fidl::Client<fuchsia_logger::LogSink> logger_client;
    logger_client.Bind(fidl::ClientEnd<fuchsia_logger::LogSink>(std::move(logger)),
                       loop_.dispatcher());
    zx::socket local, remote;
    if (zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote) != ZX_OK) {
      return;
    }
    if (logger_client->ConnectStructured(std::move(remote)).is_error()) {
      return;
    }
    descriptor_ = std::move(local);
  }
}

void SetInterestChangedListener(void (*callback)(void* context,
                                                 fuchsia_logging::LogSeverity severity),
                                void* context) {
  GlobalStateLock log_state;
  log_state->set_severity_handler(callback, context);
}

void BeginRecordInternal(LogBuffer* buffer, fuchsia_logging::LogSeverity severity,
                         cpp17::optional<cpp17::string_view> file_name, unsigned int line,
                         cpp17::optional<cpp17::string_view> msg,
                         cpp17::optional<cpp17::string_view> condition, zx_handle_t socket) {
  // Ensure we have log state
  GlobalStateLock log_state;
  // Optional so no allocation overhead
  // occurs if condition isn't set.
  std::optional<std::string> modified_msg;
  if (condition) {
    std::stringstream s;
    s << "Check failed: " << *condition << ". ";
    if (msg) {
      s << *msg;
    }
    modified_msg = s.str();
    if (severity == fuchsia_logging::LOG_FATAL) {
      // We're crashing -- so leak the string in order to prevent
      // use-after-free of the maybe_fatal_string.
      // We need this to prevent a use-after-free in FlushRecord.
      auto new_msg = new char[modified_msg->size() + 1];
      strcpy(const_cast<char*>(new_msg), modified_msg->c_str());
      msg = new_msg;
    } else {
      msg = modified_msg->data();
    }
  }
  auto* state = RecordState::CreatePtr(buffer);
  // Invoke the constructor of RecordState to construct a valid RecordState
  // inside the LogBuffer.
  new (state) RecordState;
  state->raw_severity = severity;
  if (socket == ZX_HANDLE_INVALID) {
    socket = std::get<0>(log_state->descriptor()).get();
  }
  state->socket = socket;
  if (severity == fuchsia_logging::LOG_FATAL) {
    state->maybe_fatal_string = msg->data();
  }
  state->buffer.BeginRecord(severity, file_name, line, msg, zx::unowned_socket(socket), 0, pid,
                            tid);
  for (size_t i = 0; i < log_state->tag_count(); i++) {
    state->buffer.WriteKeyValue(kTagFieldName, log_state->tags()[i]);
  }
}

void BeginRecord(LogBuffer* buffer, fuchsia_logging::LogSeverity severity, NullSafeStringView file,
                 unsigned int line, NullSafeStringView msg, NullSafeStringView condition) {
  BeginRecordInternal(buffer, severity, file, line, msg, condition, ZX_HANDLE_INVALID);
}

void BeginRecordWithSocket(LogBuffer* buffer, fuchsia_logging::LogSeverity severity,
                           NullSafeStringView file_name, unsigned int line, NullSafeStringView msg,
                           NullSafeStringView condition, zx_handle_t socket) {
  BeginRecordInternal(buffer, severity, file_name, line, msg, condition, socket);
}

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, cpp17::string_view value) {
  auto* state = RecordState::CreatePtr(buffer);
  state->buffer.WriteKeyValue(key, value);
}

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, int64_t value) {
  auto* state = RecordState::CreatePtr(buffer);
  state->buffer.WriteKeyValue(key, value);
}

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, uint64_t value) {
  auto* state = RecordState::CreatePtr(buffer);
  state->buffer.WriteKeyValue(key, value);
}

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, double value) {
  auto* state = RecordState::CreatePtr(buffer);
  state->buffer.WriteKeyValue(key, value);
}

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, bool value) {
  auto* state = RecordState::CreatePtr(buffer);
  state->buffer.WriteKeyValue(key, value);
}

bool FlushRecord(LogBuffer* buffer) {
  GlobalStateLock log_state;
  auto* state = RecordState::CreatePtr(buffer);
  if (state->raw_severity < log_state->min_severity()) {
    return true;
  }
  auto ret = state->buffer.FlushRecord();
  if (state->raw_severity == fuchsia_logging::LOG_FATAL) {
    std::cerr << state->maybe_fatal_string << std::endl;
    abort();
  }
  return ret;
}

void LogState::Set(const fuchsia_logging::LogSettings& settings, const GlobalStateLock& lock) {
  Set(settings, {}, lock);
}

void LogState::Set(const fuchsia_logging::LogSettings& settings,
                   const std::initializer_list<std::string>& tags, const GlobalStateLock& lock) {
  auto old = *lock;
  lock.Set(new LogState(settings, tags));
  if (old) {
    delete old;
  }
}

LogState::LogState(const fuchsia_logging::LogSettings& in_settings,
                   const std::initializer_list<std::string>& tags)
    : loop_(&kAsyncLoopConfigNeverAttachToThread),
      executor_(loop_.dispatcher()),
      min_severity_(in_settings.min_log_level),
      default_severity_(in_settings.min_log_level),
      wait_for_initial_interest_(in_settings.wait_for_initial_interest) {
  fuchsia_logging::LogSettings settings = in_settings;
  interest_listener_dispatcher_ =
      static_cast<async_dispatcher_t*>(settings.single_threaded_dispatcher);
  serve_interest_listener_ = !settings.disable_interest_listener;
  min_severity_ = in_settings.min_log_level;

  provided_log_sink_ = in_settings.log_sink;
  for (auto& tag : tags) {
    tags_[num_tags_++] = tag;
    if (num_tags_ >= kMaxTags)
      break;
  }
  Connect();
}

void SetLogSettings(const fuchsia_logging::LogSettings& settings) {
  GlobalStateLock lock;
  LogState::Set(settings, lock);
}

void SetLogSettings(const fuchsia_logging::LogSettings& settings,
                    const std::initializer_list<std::string>& tags) {
  GlobalStateLock lock;
  LogState::Set(settings, tags, lock);
}

fuchsia_logging::LogSeverity GetMinLogLevel() {
  GlobalStateLock lock;
  return lock->min_severity();
}

}  // namespace syslog_runtime
