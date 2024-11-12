// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <fidl/fuchsia.logger/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/executor.h>
#include <lib/async/default.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/stdcompat/variant.h>
#include <lib/sync/completion.h>
#include <lib/syslog/cpp/log_level.h>
#include <lib/syslog/cpp/log_settings.h>
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

class GlobalStateLock;
namespace internal {
class LogState {
 public:
  static void Set(const fuchsia_logging::LogSettings& settings, const GlobalStateLock& lock);

  fuchsia_logging::RawLogSeverity min_severity() const { return min_severity_; }

  const std::vector<std::string>& tags() const { return tags_; }

  // Allowed to be const because descriptor_ is mutable
  cpp17::variant<zx::socket, std::ofstream>& descriptor() const { return logsink_socket_; }

 private:
  explicit LogState(const fuchsia_logging::LogSettings& settings);

  void Connect();

  void PollInterest();

  void HandleInterest(fuchsia_diagnostics::wire::Interest interest);

  fidl::WireSharedClient<fuchsia_logger::LogSink> log_sink_;
  void (*on_severity_changed_)(fuchsia_logging::RawLogSeverity severity);
  // Loop that never runs any code, but is needed so FIDL
  // doesn't crash if we have no dispatcher thread.
  async::Loop unused_loop_;
  std::atomic<fuchsia_logging::RawLogSeverity> min_severity_;
  const fuchsia_logging::RawLogSeverity default_severity_;
  mutable cpp17::variant<zx::socket, std::ofstream> logsink_socket_ = zx::socket();
  std::vector<std::string> tags_;
  async_dispatcher_t* interest_listener_dispatcher_;
  fuchsia_logging::InterestListenerBehavior interest_listener_config_;
  // Handle to a fuchsia.logger.LogSink instance.
  cpp17::optional<fidl::ClientEnd<fuchsia_logger::LogSink>> provided_log_sink_;
};
}  // namespace internal

// Global state lock. In order to mutate the LogState through SetStateLocked
// and GetStateLocked you must hold this capability.
// Do not directly use the C API. The C API exists solely
// to expose globals as a shared library.
// If the logger is not initialized, this will implicitly init the logger.
class GlobalStateLock {
 public:
  GlobalStateLock(bool autoinit = true) {
    internal::FuchsiaLogAcquireState();
    if (autoinit && !internal::FuchsiaLogGetStateLocked()) {
      internal::LogState::Set(fuchsia_logging::LogSettings(), *this);
    }
  }

  // Retrieves the global state
  internal::LogState* operator->() const { return internal::FuchsiaLogGetStateLocked(); }

  // Sets the global state
  void Set(internal::LogState* state) const { FuchsiaLogSetStateLocked(state); }

  // Retrieves the global state
  internal::LogState* operator*() const { return internal::FuchsiaLogGetStateLocked(); }

  ~GlobalStateLock() { internal::FuchsiaLogReleaseState(); }
};
namespace {

zx_koid_t ProcessSelfKoid() {
  zx_info_handle_basic_t info;
  // We need to use _zx_object_get_info to avoid breaking the driver ABI.
  // fake_ddk can fake out this method, which results in us deadlocking
  // when used in certain drivers because the fake doesn't properly pass-through
  // to the real syscall in this case.
  zx_status_t status = _zx_object_get_info(zx_process_self(), ZX_INFO_HANDLE_BASIC, &info,
                                           sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

zx_koid_t globalPid = ProcessSelfKoid();
const char kTagFieldName[] = "tag";

void BeginRecordInternal(LogBuffer* buffer, fuchsia_logging::RawLogSeverity severity,
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
    if (severity == fuchsia_logging::LogSeverity::Fatal) {
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
  buffer->SetRawSeverity(severity);
  if (socket == ZX_HANDLE_INVALID) {
    socket = std::get<0>(log_state->descriptor()).get();
  }
  if (severity == fuchsia_logging::LogSeverity::Fatal) {
    buffer->SetFatalErrorString(msg->data());
  }
  buffer->BeginRecord(severity, file_name, line, msg, socket, 0, globalPid,
                      internal::FuchsiaLogGetCurrentThreadKoid());
  for (size_t i = 0; i < log_state->tags().size(); i++) {
    buffer->WriteKeyValue(kTagFieldName, log_state->tags()[i]);
  }
}

void BeginRecord(LogBuffer* buffer, fuchsia_logging::RawLogSeverity severity,
                 internal::NullSafeStringView file, unsigned int line,
                 internal::NullSafeStringView msg, internal::NullSafeStringView condition) {
  BeginRecordInternal(buffer, severity, file, line, msg, condition, ZX_HANDLE_INVALID);
}

void BeginRecordWithSocket(LogBuffer* buffer, fuchsia_logging::RawLogSeverity severity,
                           internal::NullSafeStringView file_name, unsigned int line,
                           internal::NullSafeStringView msg, internal::NullSafeStringView condition,
                           zx_handle_t socket) {
  BeginRecordInternal(buffer, severity, file_name, line, msg, condition, socket);
}

void SetLogSettings(const fuchsia_logging::LogSettings& settings) {
  GlobalStateLock lock(false);
  internal::LogState::Set(settings, lock);
}

fuchsia_logging::RawLogSeverity GetMinLogSeverity() {
  GlobalStateLock lock;
  return lock->min_severity();
}
}  // namespace

void internal::LogState::PollInterest() {
  log_sink_->WaitForInterestChange().Then(
      [this](fidl::WireUnownedResult<fuchsia_logger::LogSink::WaitForInterestChange>&
                 interest_result) {
        // FIDL can cancel the operation if the logger is being reconfigured
        // which results in an error.
        if (!interest_result.ok()) {
          return;
        }
        HandleInterest(interest_result->value()->data);
        on_severity_changed_(min_severity_);
        PollInterest();
      });
}

void internal::LogState::HandleInterest(fuchsia_diagnostics::wire::Interest interest) {
  if (!interest.has_min_severity()) {
    min_severity_ = default_severity_;
  } else {
    min_severity_ = static_cast<fuchsia_logging::RawLogSeverity>(interest.min_severity());
  }
}

void internal::LogState::Connect() {
  auto default_dispatcher = async_get_default_dispatcher();
  bool missing_dispatcher = false;
  if (interest_listener_config_ != fuchsia_logging::Disabled) {
    if (!interest_listener_dispatcher_ && !default_dispatcher) {
      missing_dispatcher = true;
    }
  }
  // Regardless of whether or not we need to do anything async, FIDL async bindings
  // require a valid dispatcher or they panic.
  if (!interest_listener_dispatcher_) {
    if (default_dispatcher) {
      interest_listener_dispatcher_ = default_dispatcher;
    } else {
      interest_listener_dispatcher_ = unused_loop_.dispatcher();
    }
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
  if (interest_listener_config_ == fuchsia_logging::Enabled && !missing_dispatcher) {
    auto interest_result = log_sink_.sync()->WaitForInterestChange();
    if (!interest_result.ok()) {
      // Logging isn't available. Silently drop logs.
      return;
    }
    if (interest_result->is_ok()) {
      HandleInterest(interest_result->value()->data);
    }
    on_severity_changed_(min_severity_);
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

void LogBuffer::BeginRecord(fuchsia_logging::RawLogSeverity severity,
                            cpp17::optional<cpp17::string_view> file_name, unsigned int line,
                            cpp17::optional<cpp17::string_view> message, zx_handle_t socket,
                            uint32_t dropped_count, zx_koid_t pid, zx_koid_t tid) {
  inner_.BeginRecord(severity, file_name, line, message, zx::unowned_socket(socket), dropped_count,
                     pid, tid);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, cpp17::string_view value) {
  inner_.WriteKeyValue(key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, int64_t value) {
  inner_.WriteKeyValue(key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, uint64_t value) {
  inner_.WriteKeyValue(key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, double value) {
  inner_.WriteKeyValue(key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, bool value) {
  inner_.WriteKeyValue(key, value);
}

bool LogBuffer::Flush() {
  GlobalStateLock log_state;
  if (raw_severity_ < log_state->min_severity()) {
    return true;
  }
  auto ret = inner_.FlushRecord();
  if (raw_severity_ == fuchsia_logging::LogSeverity::Fatal) {
    std::cerr << *maybe_fatal_string_ << std::endl;
    abort();
  }
  return ret;
}

void internal::LogState::Set(const fuchsia_logging::LogSettings& settings,
                             const GlobalStateLock& lock) {
  auto old = *lock;
  lock.Set(new LogState(settings));
  if (old) {
    delete old;
  }
}

internal::LogState::LogState(const fuchsia_logging::LogSettings& settings)
    : unused_loop_(&kAsyncLoopConfigNeverAttachToThread),
      min_severity_(settings.min_log_level),
      default_severity_(settings.min_log_level) {
  interest_listener_dispatcher_ =
      static_cast<async_dispatcher_t*>(settings.single_threaded_dispatcher);
  interest_listener_config_ = settings.interest_listener_config_;
  min_severity_ = settings.min_log_level;
  if (settings.log_sink) {
    provided_log_sink_ = fidl::ClientEnd<fuchsia_logger::LogSink>(zx::channel(settings.log_sink));
  }
  on_severity_changed_ = settings.severity_change_callback;
  if (!on_severity_changed_) {
    on_severity_changed_ = [](fuchsia_logging::RawLogSeverity severity) {};
  }
  for (auto& tag : settings.tags) {
    tags_.push_back(tag);
  }
  Connect();
}

LogBuffer LogBufferBuilder::Build() {
  LogBuffer buffer;
  if (socket_) {
    BeginRecordWithSocket(&buffer, severity_,
                          internal::NullSafeStringView::CreateFromOptional(file_name_), line_,
                          internal::NullSafeStringView::CreateFromOptional(msg_),
                          internal::NullSafeStringView::CreateFromOptional(condition_), socket_);
  } else {
    BeginRecord(&buffer, severity_, internal::NullSafeStringView::CreateFromOptional(file_name_),
                line_, internal::NullSafeStringView::CreateFromOptional(msg_),
                internal::NullSafeStringView::CreateFromOptional(condition_));
  }
  return buffer;
}

}  // namespace syslog_runtime

namespace fuchsia_logging {

// Sets the default log severity. If not explicitly set,
// this defaults to INFO, or to the value specified by Archivist.
LogSettingsBuilder& LogSettingsBuilder::WithMinLogSeverity(
    fuchsia_logging::RawLogSeverity min_log_level) {
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

LogSettingsBuilder& LogSettingsBuilder::WithSeverityChangedListener(
    void (*callback)(fuchsia_logging::RawLogSeverity severity)) {
  settings_.severity_change_callback = callback;
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

LogSettingsBuilder& LogSettingsBuilder::WithTags(const std::initializer_list<std::string>& tags) {
  for (auto& tag : tags) {
    settings_.tags.push_back(tag);
  }
  return *this;
}

void SetTags(const std::initializer_list<std::string>& tags) {
  fuchsia_logging::LogSettings settings;
  syslog_runtime::SetLogSettings({.min_log_level = GetMinLogSeverity(), .tags = tags});
}

// Configures the log settings.
void LogSettingsBuilder::BuildAndInitialize() { syslog_runtime::SetLogSettings(settings_); }

fuchsia_logging::RawLogSeverity GetMinLogSeverity() { return syslog_runtime::GetMinLogSeverity(); }

}  // namespace fuchsia_logging
