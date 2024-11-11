// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYSLOG_CPP_LOG_MESSAGE_IMPL_H_
#define LIB_SYSLOG_CPP_LOG_MESSAGE_IMPL_H_

#include <lib/stdcompat/optional.h>
#include <lib/stdcompat/string_view.h>
#include <lib/syslog/cpp/log_level.h>
#include <zircon/types.h>

#include <cstdint>
#ifdef __Fuchsia__
#include <lib/syslog/structured_backend/cpp/fuchsia_syslog.h>
#endif
#include <atomic>
#include <limits>
#include <sstream>

namespace syslog_runtime {

class LogBuffer;

/// Constructs a LogBuffer
class LogBufferBuilder {
 public:
  explicit LogBufferBuilder(fuchsia_logging::RawLogSeverity severity) : severity_(severity) {}

  /// Sets the file name and line number for the log message
  LogBufferBuilder& WithFile(cpp17::string_view file, unsigned int line) {
    file_name_ = file;
    line_ = line;
    return *this;
  }

  /// Sets the condition for the log message
  /// This is used by test frameworks that want
  /// to print an assertion message when a test fails.
  /// This prepends the string "Check failed: "<<condition<<". "
  /// to whatever message the user passes.
  LogBufferBuilder& WithCondition(cpp17::string_view condition) {
    condition_ = condition;
    return *this;
  }

  /// Sets the message for the log message
  LogBufferBuilder& WithMsg(cpp17::string_view msg) {
    msg_ = msg;
    return *this;
  }

#ifdef __Fuchsia__
  /// Sets the socket for the log message
  LogBufferBuilder& WithSocket(zx_handle_t socket) {
    socket_ = socket;
    return *this;
  }
#endif
  /// Builds the LogBuffer
  LogBuffer Build();

 private:
  cpp17::optional<cpp17::string_view> file_name_;
  unsigned int line_ = 0;
  cpp17::optional<cpp17::string_view> msg_;
  cpp17::optional<cpp17::string_view> condition_;
#ifdef __Fuchsia__
  zx_handle_t socket_ = ZX_HANDLE_INVALID;
#endif
  fuchsia_logging::RawLogSeverity severity_;
};

template <typename Key, typename Value>
class KeyValue final {
 public:
  constexpr KeyValue(Key key, Value value) : key_(key), value_(value) {}

  constexpr const Key& key() const { return key_; }
  constexpr const Value& value() const { return value_; }

 private:
  Key key_;
  Value value_;
};

// Opaque structure representing the backend encode state.
// This structure only has meaning to the backend and application code shouldn't
// touch these values.
class LogBuffer final {
 public:
  void BeginRecord(fuchsia_logging::RawLogSeverity severity,
                   cpp17::optional<cpp17::string_view> file_name, unsigned int line,
                   cpp17::optional<cpp17::string_view> message, zx_handle_t socket,
                   uint32_t dropped_count, zx_koid_t pid, zx_koid_t tid);

  void WriteKeyValue(cpp17::string_view key, cpp17::string_view value);

  void WriteKeyValue(cpp17::string_view key, int64_t value);

  void WriteKeyValue(cpp17::string_view key, uint64_t value);

  void WriteKeyValue(cpp17::string_view key, double value);

  void WriteKeyValue(cpp17::string_view key, bool value);

  void WriteKeyValue(cpp17::string_view key, const char* value) {
    WriteKeyValue(key, cpp17::string_view(value));
  }

  // Encodes an int8
  void Encode(KeyValue<const char*, int8_t> value) {
    Encode(KeyValue<const char*, int64_t>(value.key(), value.value()));
  }

  // Encodes an int16
  void Encode(KeyValue<const char*, int16_t> value) {
    Encode(KeyValue<const char*, int64_t>(value.key(), value.value()));
  }

  // Encodes an int32
  void Encode(KeyValue<const char*, int32_t> value) {
    Encode(KeyValue<const char*, int64_t>(value.key(), value.value()));
  }

  // Encodes an int64
  void Encode(KeyValue<const char*, int64_t> value) { WriteKeyValue(value.key(), value.value()); }

#ifdef __APPLE__
  // Encodes a size_t. On Apple Clang, size_t is a special type.
  void Encode(KeyValue<const char*, size_t> value) {
    WriteKeyValue(value.key(), static_cast<int64_t>(value.value()));
  }
#endif

  // Encodes an uint8_t
  void Encode(KeyValue<const char*, uint8_t> value) {
    Encode(KeyValue<const char*, uint64_t>(value.key(), value.value()));
  }

  // Encodes an uint16_t
  void Encode(KeyValue<const char*, uint16_t> value) {
    Encode(KeyValue<const char*, uint64_t>(value.key(), value.value()));
  }

  // Encodes a uint32_t
  void Encode(KeyValue<const char*, uint32_t> value) {
    Encode(KeyValue<const char*, uint64_t>(value.key(), value.value()));
  }

  // Encodes an uint64
  void Encode(KeyValue<const char*, uint64_t> value) { WriteKeyValue(value.key(), value.value()); }

  // Encodes a NULL-terminated C-string.
  void Encode(KeyValue<const char*, const char*> value) {
    WriteKeyValue(value.key(), value.value());
  }

  // Encodes a NULL-terminated C-string.
  void Encode(KeyValue<const char*, char*> value) { WriteKeyValue(value.key(), value.value()); }

  // Encodes a C++ std::string.
  void Encode(KeyValue<const char*, std::string> value) {
    WriteKeyValue(value.key(), value.value());
  }

  // Encodes a C++ std::string_view.
  void Encode(KeyValue<const char*, std::string_view> value) {
    WriteKeyValue(value.key(), value.value());
  }

  // Encodes a double floating point value
  void Encode(KeyValue<const char*, double> value) { WriteKeyValue(value.key(), value.value()); }

  // Encodes a floating point value
  void Encode(KeyValue<const char*, float> value) { WriteKeyValue(value.key(), value.value()); }

  // Encodes a boolean value
  void Encode(KeyValue<const char*, bool> value) { WriteKeyValue(value.key(), value.value()); }

  // Writes the log to a socket.
  bool Flush();

#ifdef __Fuchsia__
  /// Sets the raw severity
  void SetRawSeverity(fuchsia_logging::RawLogSeverity severity) { raw_severity_ = severity; }

  /// Sets a fatal error string
  void SetFatalErrorString(cpp17::string_view fatal_error_string) {
    maybe_fatal_string_ = fatal_error_string;
  }

#else
  uint64_t* data() { return data_; }

  uint64_t* record_state() { return record_state_; }

  static constexpr size_t record_state_size() { return sizeof(record_state_); }

  static constexpr size_t data_size() { return sizeof(data_); }
#endif

 private:
#ifdef __Fuchsia__
  // Message string -- valid if severity is FATAL. For FATAL
  // logs the caller is responsible for ensuring the string
  // is valid for the duration of the call (which our macros
  // will ensure for current users).
  // This will leak on usage, as the process will crash shortly afterwards.
  cpp17::optional<cpp17::string_view> maybe_fatal_string_;

  // Severity of the log message.
  fuchsia_logging::RawLogSeverity raw_severity_;
  // Underlying log buffer.
  fuchsia_syslog::LogBuffer inner_;
#else
  // Max size of log buffer. This number may change as additional fields
  // are added to the internal encoding state. It is based on trial-and-error
  // and is adjusted when compilation fails due to it not being large enough.
  static constexpr auto kBufferSize = (1 << 15) / 8;
  // Additional storage for internal log state.
  static constexpr auto kStateSize = 18;
  // Record state (for keeping track of backend-specific details)
  uint64_t record_state_[kStateSize];
  // Log data (used by the backend to encode the log into). The format
  // for this is backend-specific.
  uint64_t data_[kBufferSize];
#endif
};

namespace internal {
// A null-safe wrapper around cpp17::optional<cpp17::string_view>
//
// This class is used to represent a string that may be nullptr. It is used
// to avoid the need to check for nullptr before passing a string to the
// syslog macros.
//
// This class is implicitly convertible to cpp17::optional<cpp17::string_view>.
// NOLINT is used as implicit conversions are intentional here.
class NullSafeStringView final {
 public:
  //  Constructs a NullSafeStringView from a cpp17::string_view.
  constexpr NullSafeStringView(cpp17::string_view string_view)
      : string_view_(string_view) {}  // NOLINT

  // Constructs a NullSafeStringView from a nullptr.
  constexpr NullSafeStringView(std::nullptr_t) : string_view_(cpp17::nullopt) {}  // NOLINT

  constexpr NullSafeStringView(const NullSafeStringView&) = default;

  // Constructs a NullSafeStringView from a const char* which may be nullptr.
  // string Nullable string to construct from.
  constexpr NullSafeStringView(const char* input) {  // NOLINT
    if (!input) {
      string_view_ = cpp17::nullopt;
    } else {
      string_view_ = cpp17::string_view(input);
    }
  }

  // Creates a NullSafeStringView fro, an optional<cpp17::string_view>.
  // This is not a constructor to prevent accidental misuse.
  static NullSafeStringView CreateFromOptional(cpp17::optional<cpp17::string_view> string_view) {
    if (!string_view) {
      return NullSafeStringView(nullptr);
    }
    return NullSafeStringView(*string_view);
  }

  // Constructs a NullSafeStringView from an std::string.
  constexpr NullSafeStringView(const std::string& input) : string_view_(input) {}  // NOLINT

  // Converts this NullSafeStringView to a cpp17::optional<cpp17::string_view>.
  constexpr operator cpp17::optional<cpp17::string_view>() const { return string_view_; }  // NOLINT
 private:
  cpp17::optional<cpp17::string_view> string_view_;
};

template <typename Msg, typename... Args>
void WriteStructuredLog(fuchsia_logging::LogSeverity severity, const char* file, int line, Msg msg,
                        Args... args) {
  syslog_runtime::LogBufferBuilder builder(severity);
  if (file) {
    builder.WithFile(file, line);
  }
  if (msg != nullptr) {
    builder.WithMsg(msg);
  }
  auto buffer = builder.Build();
  (void)std::initializer_list<int>{(buffer.Encode(args), 0)...};
  buffer.Flush();
}
}  // namespace internal
}  // namespace syslog_runtime

namespace fuchsia_logging {

class LogMessageVoidify final {
 public:
  void operator&(std::ostream&) {}
};

class LogMessage final {
 public:
  LogMessage(RawLogSeverity severity, const char* file, int line, const char* condition,
             const char* tag
#if defined(__Fuchsia__)
             ,
             zx_status_t status = std::numeric_limits<zx_status_t>::max()
#endif
  );
  ~LogMessage();

  std::ostream& stream() { return stream_; }

 private:
  std::ostringstream stream_;
  const RawLogSeverity severity_;
  const char* file_;
  const int line_;
  const char* condition_;
  const char* tag_;
#if defined(__Fuchsia__)
  const zx_status_t status_;
#endif
};

// LogFirstNState is used by the macro FX_LOGS_FIRST_N below.
class LogFirstNState final {
 public:
  bool ShouldLog(uint32_t n);

 private:
  std::atomic<uint32_t> counter_{0};
};

/// Returns true if |severity| is at or above the current minimum log level.
/// LOG_FATAL and above is always true.
bool IsSeverityEnabled(RawLogSeverity severity);

}  // namespace fuchsia_logging

#endif  // LIB_SYSLOG_CPP_LOG_MESSAGE_IMPL_H_
