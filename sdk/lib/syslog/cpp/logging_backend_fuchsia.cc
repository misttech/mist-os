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

#include "lib/component/incoming/cpp/protocol.h"
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
  cpp17::variant<zx::socket, std::ofstream>& descriptor() const { return logsink_socket_; }

 private:
  LogState(const fuchsia_logging::LogSettings& settings,
           const std::initializer_list<std::string>& tags);

  void Connect();

  void PollInterest();

  void HandleInterest(fuchsia_diagnostics::wire::Interest interest);

  fidl::WireSharedClient<fuchsia_logger::LogSink> log_sink_;
  void (*handler_)(void* context, fuchsia_logging::LogSeverity severity);
  void* handler_context_;
  async::Loop loop_;
  std::atomic<fuchsia_logging::LogSeverity> min_severity_;
  const fuchsia_logging::LogSeverity default_severity_;
  mutable cpp17::variant<zx::socket, std::ofstream> logsink_socket_ = zx::socket();
  std::string tags_[kMaxTags];
  size_t num_tags_ = 0;
  async_dispatcher_t* interest_listener_dispatcher_;
  fuchsia_logging::InterestListenerBehavior interest_listener_config_;
  // Handle to a fuchsia.logger.LogSink instance.
  cpp17::optional<fidl::ClientEnd<fuchsia_logger::LogSink>> provided_log_sink_;
};

// Global state lock. In order to mutate the LogState through SetStateLocked
// and GetStateLocked you must hold this capability.
// Do not directly use the C API. The C API exists solely
// to expose globals as a shared library.
// If the logger is not initialized, this will implicitly init the logger.
class GlobalStateLock {
 public:
  GlobalStateLock(bool autoinit = true) {
    FuchsiaLogAcquireState();
    if (autoinit && !FuchsiaLogGetStateLocked()) {
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

static fuchsia_logging::LogSeverity IntoLogSeverity(fuchsia_diagnostics::wire::Severity severity) {
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

void LogState::PollInterest() {
  log_sink_->WaitForInterestChange().Then(
      [=](fidl::WireUnownedResult<fuchsia_logger::LogSink::WaitForInterestChange>&
              interest_result) {
        // FIDL can cancel the operation if the logger is being reconfigured
        // which results in an error.
        if (!interest_result.ok()) {
          return;
        }
        HandleInterest(interest_result->value()->data);
        handler_(handler_context_, min_severity_);
        PollInterest();
      });
}

void LogState::HandleInterest(fuchsia_diagnostics::wire::Interest interest) {
  if (!interest.has_min_severity()) {
    min_severity_ = default_severity_;
  } else {
    min_severity_ = IntoLogSeverity(interest.min_severity());
  }
}

void LogState::Connect() {
  if (interest_listener_config_ != fuchsia_logging::Disabled) {
    if (!interest_listener_dispatcher_) {
      loop_.StartThread("log-interest-listener-thread");
    }
    handler_ = [](void* ctx, fuchsia_logging::LogSeverity severity) {};
  }
  // Regardless of whether or not we need to do anything async, FIDL async bindings
  // require a valid dispatcher or they panic.
  if (!interest_listener_dispatcher_) {
    interest_listener_dispatcher_ = loop_.dispatcher();
  }
  fidl::ClientEnd<fuchsia_logger::LogSink> client_end;
  if (!provided_log_sink_.has_value()) {
    auto connect_result = component::Connect<fuchsia_logger::LogSink>();
    if (connect_result.is_error()) {
      return;
    }
    client_end = std::move(connect_result.value());
  } else {
    client_end = std::move(*provided_log_sink_);
  }
  log_sink_.Bind(std::move(client_end), interest_listener_dispatcher_);
  if (interest_listener_config_ == fuchsia_logging::Enabled) {
    auto interest_result = log_sink_.sync()->WaitForInterestChange();
    if (interest_result->is_ok()) {
      HandleInterest(interest_result->value()->data);
    }
    handler_(handler_context_, min_severity_);
  }

  zx::socket local, remote;
  if (zx::socket::create(ZX_SOCKET_DATAGRAM, &local, &remote) != ZX_OK) {
    return;
  }
  if (!log_sink_->ConnectStructured(std::move(remote)).ok()) {
    return;
  }
  if (interest_listener_config_ != fuchsia_logging::Disabled) {
    PollInterest();
  }
  logsink_socket_ = std::move(local);
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
  buffer->raw_severity = severity;
  if (socket == ZX_HANDLE_INVALID) {
    socket = std::get<0>(log_state->descriptor()).get();
  }
  if (severity == fuchsia_logging::LOG_FATAL) {
    buffer->maybe_fatal_string = msg->data();
  }
  buffer->inner.BeginRecord(severity, file_name, line, msg, zx::unowned_socket(socket), 0, pid,
                            tid);
  for (size_t i = 0; i < log_state->tag_count(); i++) {
    buffer->inner.WriteKeyValue(kTagFieldName, log_state->tags()[i]);
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

void LogBuffer::WriteKeyValue(cpp17::string_view key, cpp17::string_view value) {
  inner.WriteKeyValue(key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, int64_t value) {
  inner.WriteKeyValue(key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, uint64_t value) {
  inner.WriteKeyValue(key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, double value) {
  inner.WriteKeyValue(key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, bool value) {
  inner.WriteKeyValue(key, value);
}

bool LogBuffer::Flush() {
  GlobalStateLock log_state;
  if (raw_severity < log_state->min_severity()) {
    return true;
  }
  auto ret = inner.FlushRecord();
  if (raw_severity == fuchsia_logging::LOG_FATAL) {
    std::cerr << *maybe_fatal_string << std::endl;
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

LogState::LogState(const fuchsia_logging::LogSettings& settings,
                   const std::initializer_list<std::string>& tags)
    : loop_(&kAsyncLoopConfigNeverAttachToThread),
      min_severity_(settings.min_log_level),
      default_severity_(settings.min_log_level) {
  interest_listener_dispatcher_ =
      static_cast<async_dispatcher_t*>(settings.single_threaded_dispatcher);
  interest_listener_config_ = settings.interest_listener_config_;
  min_severity_ = settings.min_log_level;
  if (settings.log_sink) {
    provided_log_sink_ = fidl::ClientEnd<fuchsia_logger::LogSink>(zx::channel(settings.log_sink));
  }
  for (auto& tag : tags) {
    tags_[num_tags_++] = tag;
    if (num_tags_ >= kMaxTags)
      break;
  }
  Connect();
}

void SetLogSettings(const fuchsia_logging::LogSettings& settings) {
  GlobalStateLock lock(false);
  LogState::Set(settings, lock);
}

void SetLogSettings(const fuchsia_logging::LogSettings& settings,
                    const std::initializer_list<std::string>& tags) {
  GlobalStateLock lock(false);
  LogState::Set(settings, tags, lock);
}

fuchsia_logging::LogSeverity GetMinLogSeverity() {
  GlobalStateLock lock;
  return lock->min_severity();
}

}  // namespace syslog_runtime

namespace fuchsia_logging {

// Sets the default log severity. If not explicitly set,
// this defaults to INFO, or to the value specified by Archivist.
LogSettingsBuilder& LogSettingsBuilder::WithMinLogSeverity(LogSeverity min_log_level) {
  settings_.min_log_level = min_log_level;
  return *this;
}

// Disables the interest listener.
LogSettingsBuilder& LogSettingsBuilder::DisableInterestListener() {
  WithInterestListenerConfiguration(fuchsia_logging::Disabled);
  return *this;
}

// Disables waiting for the initial interest from Archivist.
// The level specified in SetMinLogSeverity or INFO will be used
// as the default.
LogSettingsBuilder& LogSettingsBuilder::DisableWaitForInitialInterest() {
  if (settings_.interest_listener_config_ == fuchsia_logging::Enabled) {
    WithInterestListenerConfiguration(fuchsia_logging::EnabledNonBlocking);
  }
  return *this;
}

LogSettingsBuilder& LogSettingsBuilder::WithInterestListenerConfiguration(
    InterestListenerBehavior config) {
  settings_.interest_listener_config_ = config;
  return *this;
}
// Sets the log sink handle.
LogSettingsBuilder& LogSettingsBuilder::WithLogSink(zx_handle_t log_sink) {
  settings_.log_sink = log_sink;
  return *this;
}

// Sets the dispatcher to use.
LogSettingsBuilder& LogSettingsBuilder::WithDispatcher(async_dispatcher_t* dispatcher) {
  settings_.single_threaded_dispatcher = dispatcher;
  return *this;
}

// Configures the log settings with the specified tags.
void LogSettingsBuilder::BuildAndInitializeWithTags(
    const std::initializer_list<std::string>& tags) {
  syslog_runtime::SetLogSettings(settings_, tags);
}

void SetTags(const std::initializer_list<std::string>& tags) {
  syslog_runtime::SetLogSettings({.min_log_level = GetMinLogSeverity()}, tags);
}

// Configures the log settings.
void LogSettingsBuilder::BuildAndInitialize() { syslog_runtime::SetLogSettings(settings_); }

fuchsia_logging::LogSeverity GetMinLogSeverity() { return syslog_runtime::GetMinLogSeverity(); }

}  // namespace fuchsia_logging
