// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <fuchsia/diagnostics/stream/cpp/fidl.h>
#include <fuchsia/logger/cpp/fidl.h>
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
#include <lib/zx/channel.h>
#include <lib/zx/process.h>

#include <cstddef>
#include <fstream>
#include <iostream>
#include <sstream>

#include "lib/syslog/cpp/macros.h"
#include "sdk/lib/syslog/structured_backend/cpp/fuchsia_syslog.h"

namespace syslog_backend {

// Returns true if we are running in the DDK.
// This is used to customize logging behavior for drivers
// and related driver functionality.
bool fx_log_compat_no_interest_listener();

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
static thread_local zx_koid_t tid = GetCurrentThreadKoid();

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

  fuchsia::logger::LogSinkPtr log_sink_;
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
    AcquireState();
    if (!GetStateLocked()) {
      LogState::Set(fuchsia_logging::LogSettings(), *this);
    }
  }

  // Retrieves the global state
  syslog_backend::LogState* operator->() const { return GetStateLocked(); }

  // Sets the global state
  void Set(syslog_backend::LogState* state) const { SetStateLocked(state); }

  // Retrieves the global state
  syslog_backend::LogState* operator*() const { return GetStateLocked(); }

  ~GlobalStateLock() { ReleaseState(); }
};

static fuchsia_logging::LogSeverity IntoLogSeverity(fuchsia::diagnostics::Severity severity) {
  switch (severity) {
    case fuchsia::diagnostics::Severity::TRACE:
      return fuchsia_logging::LOG_TRACE;
    case fuchsia::diagnostics::Severity::DEBUG:
      return fuchsia_logging::LOG_DEBUG;
    case fuchsia::diagnostics::Severity::INFO:
      return fuchsia_logging::LOG_INFO;
    case fuchsia::diagnostics::Severity::WARN:
      return fuchsia_logging::LOG_WARNING;
    case fuchsia::diagnostics::Severity::ERROR:
      return fuchsia_logging::LOG_ERROR;
    case fuchsia::diagnostics::Severity::FATAL:
      return fuchsia_logging::LOG_FATAL;
  }
}

void LogState::HandleInterest() {
  log_sink_->WaitForInterestChange(
      [=](fuchsia::logger::LogSink_WaitForInterestChange_Result interest_result) {
        auto interest = std::move(interest_result.response().data);
        if (!interest.has_min_severity()) {
          min_severity_ = default_severity_;
        } else {
          min_severity_ = IntoLogSeverity(interest.min_severity());
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
    // TODO(https://fxbug.dev/75214): Support for custom names.
    if (fdio_service_connect("/svc/fuchsia.logger.LogSink", logger_request.release()) != ZX_OK) {
      return;
    }
  } else {
    logger = zx::channel(provided_log_sink_);
    provided_log_sink_ = ZX_HANDLE_INVALID;
  }

  if (wait_for_initial_interest_) {
    fuchsia::logger::LogSinkSyncPtr sync_log_sink;
    sync_log_sink.Bind(std::move(logger));
    fuchsia::logger::LogSink_WaitForInterestChange_Result interest_result;
    sync_log_sink->WaitForInterestChange(&interest_result);
    auto interest = std::move(interest_result.response().data);
    if (!interest.has_min_severity()) {
      min_severity_ = default_severity_;
    } else {
      min_severity_ = IntoLogSeverity(interest.min_severity());
    }
    handler_(handler_context_, min_severity_);
    logger = sync_log_sink.Unbind().TakeChannel();
  }

  if (log_sink_.Bind(std::move(logger), loop_.dispatcher()) != ZX_OK) {
    return;
  }
  zx::socket local, remote;
  if (zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote) != ZX_OK) {
    return;
  }
  log_sink_->ConnectStructured(std::move(remote));
  HandleInterest();
  descriptor_ = std::move(local);
}

void LogState::Connect() {
  // Always disable the interest listener for the DDK.
  // Once structured logging is available in the SDK
  // this may unblock support for this.
  // For now -- we have no way to properly support
  // this functionality for drivers.
  if (fx_log_compat_no_interest_listener()) {
    serve_interest_listener_ = false;
  }
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
      // TODO(https://fxbug.dev/75214): Support for custom names.
      if (fdio_service_connect("/svc/fuchsia.logger.LogSink", logger_request.release()) != ZX_OK) {
        return;
      }
    } else {
      logger = zx::channel(provided_log_sink_);
    }
    ::fuchsia::logger::LogSink_SyncProxy logger_client(std::move(logger));
    zx::socket local, remote;
    if (zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote) != ZX_OK) {
      return;
    }
    if (logger_client.ConnectStructured(std::move(remote))) {
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

static cpp17::optional<cpp17::string_view> CStringToStringView(const char* str) {
  if (!str) {
    return cpp17::nullopt;
  }
  return cpp17::string_view(str, strlen(str));
}

void BeginRecordInternal(LogBuffer* buffer, fuchsia_logging::LogSeverity severity,
                         const char* file_name, unsigned int line, const char* msg,
                         const char* condition, bool is_printf, zx_handle_t socket) {
  // Ensure we have log state
  GlobalStateLock log_state;
  // Optional so no allocation overhead
  // occurs if condition isn't set.
  std::optional<std::string> modified_msg;
  if (condition) {
    std::stringstream s;
    s << "Check failed: " << condition << ". ";
    if (msg) {
      s << msg;
    }
    modified_msg = s.str();
    if (severity == fuchsia_logging::LOG_FATAL) {
      // We're crashing -- so leak the string in order to prevent
      // use-after-free of the maybe_fatal_string.
      // We need this to prevent a use-after-free in FlushRecord.
      msg = new char[modified_msg->size() + 1];
      strcpy(const_cast<char*>(msg), modified_msg->c_str());
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
    state->maybe_fatal_string = msg;
  }
  state->buffer.BeginRecord(severity, CStringToStringView(file_name), line,
                            CStringToStringView(msg), is_printf, zx::unowned_socket(socket), 0, pid,
                            tid);
  for (size_t i = 0; i < log_state->tag_count(); i++) {
    state->buffer.WriteKeyValue(kTagFieldName, log_state->tags()[i]);
  }
}

void BeginRecordPrintf(LogBuffer* buffer, fuchsia_logging::LogSeverity severity,
                       const char* file_name, unsigned int line, const char* msg) {
  BeginRecordInternal(buffer, severity, file_name, line, msg, nullptr, true /* is_printf */,
                      ZX_HANDLE_INVALID);
}

void BeginRecord(LogBuffer* buffer, fuchsia_logging::LogSeverity severity, const char* file_name,
                 unsigned int line, const char* msg, const char* condition) {
  BeginRecordInternal(buffer, severity, file_name, line, msg, condition, false /* is_printf */,
                      ZX_HANDLE_INVALID);
}

void BeginRecordWithSocket(LogBuffer* buffer, fuchsia_logging::LogSeverity severity,
                           const char* file_name, unsigned int line, const char* msg,
                           const char* condition, zx_handle_t socket) {
  BeginRecordInternal(buffer, severity, file_name, line, msg, condition, false /* is_printf */,
                      socket);
}

void WriteKeyValue(LogBuffer* buffer, const char* key, const char* value) {
  WriteKeyValue(buffer, key, value, strlen(value));
}

void WriteKeyValue(LogBuffer* buffer, const char* key, const char* value, size_t value_length) {
  auto* state = RecordState::CreatePtr(buffer);
  state->buffer.WriteKeyValue(key, cpp17::string_view(value, value_length));
}

void WriteKeyValue(LogBuffer* buffer, const char* key, int64_t value) {
  auto* state = RecordState::CreatePtr(buffer);
  state->buffer.WriteKeyValue(key, value);
}

void WriteKeyValue(LogBuffer* buffer, const char* key, uint64_t value) {
  auto* state = RecordState::CreatePtr(buffer);
  state->buffer.WriteKeyValue(key, value);
}

void WriteKeyValue(LogBuffer* buffer, const char* key, double value) {
  auto* state = RecordState::CreatePtr(buffer);
  state->buffer.WriteKeyValue(key, value);
}

void WriteKeyValue(LogBuffer* buffer, const char* key, bool value) {
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

}  // namespace syslog_backend
