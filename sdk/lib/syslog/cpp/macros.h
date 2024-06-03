// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYSLOG_CPP_MACROS_H_
#define LIB_SYSLOG_CPP_MACROS_H_

#include <lib/stdcompat/optional.h>
#include <lib/stdcompat/string_view.h>
#include <lib/syslog/cpp/log_level.h>
#include <zircon/types.h>

#include <atomic>
#include <functional>
#include <limits>
#include <sstream>
#include <vector>

namespace syslog_runtime {

struct LogBuffer;

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

  // Constructs a NullSafeStringView from an std::string.
  constexpr NullSafeStringView(const std::string& input) : string_view_(input) {}  // NOLINT

  // Converts this NullSafeStringView to a cpp17::optional<cpp17::string_view>.
  constexpr operator cpp17::optional<cpp17::string_view>() const { return string_view_; }  // NOLINT
 private:
  cpp17::optional<cpp17::string_view> string_view_;
};

#ifdef __Fuchsia__
void BeginRecordWithSocket(LogBuffer* buffer, fuchsia_logging::LogSeverity severity,
                           NullSafeStringView file_name, unsigned int line, NullSafeStringView msg,
                           NullSafeStringView condition, zx_handle_t socket);
void SetInterestChangedListener(void (*callback)(void* context,
                                                 fuchsia_logging::LogSeverity severity),
                                void* context);
#endif
void BeginRecord(LogBuffer* buffer, fuchsia_logging::LogSeverity severity, NullSafeStringView file,
                 unsigned int line, NullSafeStringView msg, NullSafeStringView condition);

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, cpp17::string_view value);

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, int64_t value);

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, uint64_t value);

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, double value);

void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, bool value);

static void WriteKeyValue(LogBuffer* buffer, cpp17::string_view key, const char* value) {
  WriteKeyValue(buffer, key, cpp17::string_view(value));
}

bool FlushRecord(LogBuffer* buffer);

template <typename... Args>
constexpr size_t ArgsSize(Args... args) {
  return sizeof...(args);
}

template <typename... Args>
struct Tuplet final {
  std::tuple<Args...> tuple;
  size_t size;
  constexpr Tuplet(std::tuple<Args...> tuple, size_t size) : tuple(tuple), size(size) {}
};

template <typename Key, typename Value>
struct KeyValue final {
  Key key;
  Value value;
  constexpr KeyValue(Key key, Value value) : key(key), value(value) {}
};

// Opaque structure representing the backend encode state.
// This structure only has meaning to the backend and application code shouldn't
// touch these values.
struct LogBuffer final {
  // Max size of log buffer. This number may change as additional fields
  // are added to the internal encoding state. It is based on trial-and-error
  // and is adjusted when compilation fails due to it not being large enough.
  static constexpr auto kBufferSize = (1 << 15) / 8;
  // Additional storage for internal log state.
  static constexpr auto kStateSize = 18;
  // Record state (for keeping track of backend-specific details)
  uint64_t record_state[kStateSize];
  // Log data (used by the backend to encode the log into). The format
  // for this is backend-specific.
  uint64_t data[kBufferSize];

  // Encodes an int8
  void Encode(KeyValue<const char*, int8_t> value) {
    Encode(KeyValue<const char*, int64_t>(value.key, value.value));
  }

  // Encodes an int16
  void Encode(KeyValue<const char*, int16_t> value) {
    Encode(KeyValue<const char*, int64_t>(value.key, value.value));
  }

  // Encodes an int32
  void Encode(KeyValue<const char*, int32_t> value) {
    Encode(KeyValue<const char*, int64_t>(value.key, value.value));
  }

  // Encodes an int64
  void Encode(KeyValue<const char*, int64_t> value) {
    syslog_runtime::WriteKeyValue(this, value.key, value.value);
  }

#ifdef __APPLE__
  // Encodes a size_t. On Apple Clang, size_t is a special type.
  void Encode(KeyValue<const char*, size_t> value) {
    syslog_runtime::WriteKeyValue(this, value.key, static_cast<int64_t>(value.value));
  }
#endif

  // Encodes an uint8_t
  void Encode(KeyValue<const char*, uint8_t> value) {
    Encode(KeyValue<const char*, uint64_t>(value.key, value.value));
  }

  // Encodes an uint16_t
  void Encode(KeyValue<const char*, uint16_t> value) {
    Encode(KeyValue<const char*, uint64_t>(value.key, value.value));
  }

  // Encodes a uint32_t
  void Encode(KeyValue<const char*, uint32_t> value) {
    Encode(KeyValue<const char*, uint64_t>(value.key, value.value));
  }

  // Encodes an uint64
  void Encode(KeyValue<const char*, uint64_t> value) {
    syslog_runtime::WriteKeyValue(this, value.key, value.value);
  }

  // Encodes a NULL-terminated C-string.
  void Encode(KeyValue<const char*, const char*> value) {
    syslog_runtime::WriteKeyValue(this, value.key, value.value);
  }

  // Encodes a NULL-terminated C-string.
  void Encode(KeyValue<const char*, char*> value) {
    syslog_runtime::WriteKeyValue(this, value.key, value.value);
  }

  // Encodes a C++ std::string.
  void Encode(KeyValue<const char*, std::string> value) {
    syslog_runtime::WriteKeyValue(this, value.key, value.value);
  }

  // Encodes a C++ std::string_view.
  void Encode(KeyValue<const char*, std::string_view> value) {
    syslog_runtime::WriteKeyValue(this, value.key, value.value);
  }

  // Encodes a double floating point value
  void Encode(KeyValue<const char*, double> value) {
    syslog_runtime::WriteKeyValue(this, value.key, value.value);
  }

  // Encodes a floating point value
  void Encode(KeyValue<const char*, float> value) {
    syslog_runtime::WriteKeyValue(this, value.key, value.value);
  }

  // Encodes a boolean value
  void Encode(KeyValue<const char*, bool> value) {
    syslog_runtime::WriteKeyValue(this, value.key, value.value);
  }
};
}  // namespace syslog_runtime

namespace fuchsia_logging {

// Used to denote a key-value pair for use in structured logging API calls.
// This macro exists solely to improve readability of calls to FX_SLOG
#define FX_KV(a, b) ::syslog_runtime::KeyValue(a, b)

class LogMessageVoidify final {
 public:
  void operator&(std::ostream&) {}
};

class LogMessage final {
 public:
  LogMessage(LogSeverity severity, const char* file, int line, const char* condition,
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
  const LogSeverity severity_;
  const char* file_;
  const int line_;
  const char* condition_;
  const char* tag_;
#if defined(__Fuchsia__)
  const zx_status_t status_;
#endif
};

// LogFirstNState is used by the macro FX_SLOG_FIRST_N_SECONDS below.
class LogEveryNSecondsState final {
 public:
  bool ShouldLog(uint32_t n);
  uint32_t GetCounter();

 private:
  std::chrono::high_resolution_clock::time_point GetCurrentTime();
  bool ShouldLogInternal(uint32_t n);

  std::atomic<uint32_t> counter_{0};
  std::chrono::high_resolution_clock::time_point last_;
};

// LogFirstNState is used by the macro FX_LOGS_FIRST_N below.
class LogFirstNState final {
 public:
  bool ShouldLog(uint32_t n);

 private:
  std::atomic<uint32_t> counter_{0};
};

// Gets the FX_VLOGS default verbosity level.
uint8_t GetVlogVerbosity();

// Returns true if |severity| is at or above the current minimum log level.
// LOG_FATAL and above is always true.
bool ShouldCreateLogMessage(LogSeverity severity);

}  // namespace fuchsia_logging

#define FX_LOG_STREAM(severity, tag)                                                            \
  ::fuchsia_logging::LogMessage(::fuchsia_logging::LOG_##severity, __FILE__, __LINE__, nullptr, \
                                tag)                                                            \
      .stream()

#define FX_LOG_STREAM_STATUS(severity, status, tag)                                             \
  ::fuchsia_logging::LogMessage(::fuchsia_logging::LOG_##severity, __FILE__, __LINE__, nullptr, \
                                tag, status)                                                    \
      .stream()

#define FX_LAZY_STREAM(stream, condition) \
  !(condition) ? (void)0 : ::fuchsia_logging::LogMessageVoidify() & (stream)

#define FX_EAT_STREAM_PARAMETERS(ignored)                                                       \
  true || (ignored)                                                                             \
      ? (void)0                                                                                 \
      : ::fuchsia_logging::LogMessageVoidify() &                                                \
            ::fuchsia_logging::LogMessage(::fuchsia_logging::LOG_FATAL, 0, 0, nullptr, nullptr) \
                .stream()

#define FX_LOG_IS_ON(severity) \
  (::fuchsia_logging::ShouldCreateLogMessage(::fuchsia_logging::LOG_##severity))

#define FX_LOGS(severity) FX_LOGST(severity, nullptr)

#define FX_LOGST(severity, tag) FX_LAZY_STREAM(FX_LOG_STREAM(severity, tag), FX_LOG_IS_ON(severity))

#if defined(__Fuchsia__)
#define FX_PLOGST(severity, tag, status) \
  FX_LAZY_STREAM(FX_LOG_STREAM_STATUS(severity, status, tag), FX_LOG_IS_ON(severity))
#define FX_PLOGS(severity, status) FX_PLOGST(severity, nullptr, status)
#endif

// Writes a message to the global logger, the first |n| times that any callsite
// of this macro is invoked. |n| should be a positive integer literal.
// |severity| is one of INFO, WARNING, ERROR, FATAL
//
// Implementation notes:
// The outer for loop is a trick to allow us to introduce a new scope and
// introduce the variable |do_log| into that scope. It executes exactly once.
//
// The inner for loop is a trick to allow us to introduce a new scope and
// introduce the static variable |internal_state| into that new scope. It
// executes either zero or one times.
//
// C++ does not allow us to introduce two new variables into a single for loop
// scope and we need |do_log| so that the inner for loop doesn't execute twice.
#define FX_FIRST_N(n, log_statement)                              \
  for (bool do_log = true; do_log; do_log = false)                \
    for (static ::fuchsia_logging::LogFirstNState internal_state; \
         do_log && internal_state.ShouldLog(n); do_log = false)   \
  log_statement
#define FX_LOGS_FIRST_N(severity, n) FX_FIRST_N(n, FX_LOGS(severity))
#define FX_LOGST_FIRST_N(severity, n, tag) FX_FIRST_N(n, FX_LOGST(severity, tag))

#define FX_FIRST_N_SECS(n, log_statement)                                                 \
  for (bool do_log = true; do_log; do_log = false)                                        \
    for (static ::fuchsia_logging::LogEveryNSecondsState internal_state;                  \
         do_log && internal_state.ShouldLog(n); do_log = false)                           \
      for ([[maybe_unused]] const uint32_t COUNTER = internal_state.GetCounter(); do_log; \
           do_log = false)                                                                \
  log_statement
#define FX_SLOG_EVERY_N_SECONDS(severity, n, msg...) FX_FIRST_N_SECS(n, FX_SLOG(severity, msg))

#define FX_CHECK(condition) FX_CHECKT(condition, nullptr)

#define FX_CHECKT(condition, tag)                                                                \
  FX_LAZY_STREAM(::fuchsia_logging::LogMessage(::fuchsia_logging::LOG_FATAL, __FILE__, __LINE__, \
                                               #condition, tag)                                  \
                     .stream(),                                                                  \
                 !(condition))

// The VLOG macros log with translated verbosities

// Get the severity corresponding to the given verbosity. Note that
// verbosity relative to the default severity and can be thought of
// as incrementally "more vebose than" the baseline.
fuchsia_logging::LogSeverity GetSeverityFromVerbosity(uint8_t verbosity);

#ifndef NDEBUG
#define FX_DLOGS(severity) FX_LOGS(severity)
#define FX_DCHECK(condition) FX_CHECK(condition)
#else
#define FX_DLOGS(severity) FX_EAT_STREAM_PARAMETERS(true)
#define FX_DCHECK(condition) FX_EAT_STREAM_PARAMETERS(condition)
#endif

#define FX_NOTREACHED() FX_DCHECK(false)

#define FX_NOTIMPLEMENTED() FX_LOGS(ERROR) << "Not implemented in: " << __PRETTY_FUNCTION__

template <typename Msg, typename... Args>
void fx_slog_internal(fuchsia_logging::LogSeverity flag, const char* file, int line, Msg msg,
                      Args... args) {
  syslog_runtime::LogBuffer buffer;
  syslog_runtime::BeginRecord(&buffer, flag, file, line, msg, nullptr);
  (void)std::initializer_list<int>{(buffer.Encode(args), 0)...};
  syslog_runtime::FlushRecord(&buffer);
}

#define FX_SLOG_ETC(flag, args...)                         \
  do {                                                     \
    if (::fuchsia_logging::ShouldCreateLogMessage(flag)) { \
      fx_slog_internal(flag, __FILE__, __LINE__, args);    \
    }                                                      \
  } while (0)

#define FX_SLOG(flag, msg...) FX_SLOG_ETC(::fuchsia_logging::LOG_##flag, msg)

#endif  // LIB_SYSLOG_CPP_MACROS_H_
