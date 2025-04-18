// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_LOGGING_CPP_LOGGER_H_
#define LIB_DRIVER_LOGGING_CPP_LOGGER_H_

#include <fidl/fuchsia.logger/cpp/markers.h>
#include <fidl/fuchsia.logger/cpp/wire_types.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/syslog/structured_backend/cpp/fuchsia_syslog.h>
#include <lib/syslog/structured_backend/fuchsia_syslog.h>
#include <lib/zx/socket.h>

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD) && __cplusplus >= 202002L
#include <format>
#include <source_location>

#include "internal/panic.h"
#endif

#define FDF_LOGL(severity, logger, msg...) \
  (logger).logf((FUCHSIA_LOG_##severity), nullptr, __FILE__, __LINE__, msg)

#define FDF_LOG(severity, msg...) FDF_LOGL(severity, *fdf::Logger::GlobalInstance(), msg)

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD) && __cplusplus >= 202002L

#define FDF_ASSERT(x)                                                     \
  do {                                                                    \
    if (x) [[likely]] {                                                   \
      break;                                                              \
    } else {                                                              \
      fdf::panic("ASSERT FAILED at ({}:{}): {}", __FILE__, __LINE__, #x); \
    }                                                                     \
  } while (0)

#define FDF_ASSERT_MSG(x, msg, msgargs...)                                               \
  do {                                                                                   \
    if (x) [[likely]] {                                                                  \
      break;                                                                             \
    } else {                                                                             \
      fdf::panic("ASSERT FAILED at ({}:{}): {}" msg, __FILE__, __LINE__, #x, ##msgargs); \
    }                                                                                    \
  } while (0)
#endif

namespace fdf {

enum LogSeverity : uint8_t {
  TRACE = FUCHSIA_LOG_TRACE,
  DEBUG = FUCHSIA_LOG_DEBUG,
  INFO = FUCHSIA_LOG_INFO,
  WARN = FUCHSIA_LOG_WARNING,
  ERROR = FUCHSIA_LOG_ERROR,
  FATAL = FUCHSIA_LOG_FATAL,
};

// Provides a driver's logger.
class Logger final {
 public:
  // Creates a logger with a given |name|, which will only send logs that are of
  // at least |min_severity|.
  //
  // |dispatcher| must be single threaded or synchornized. Create must be called from the context of
  // the |dispatcher|.
  //
  // If |wait_for_initial_interest| is true we this will synchronously query the
  // fuchsia.logger/LogSink for the min severity it should expect, overriding the min_severity
  // supplied.
  //
  // If we fail to connect to LogSink, or if there's any error the returned logger will be no-op.
#if FUCHSIA_API_LEVEL_AT_LEAST(24)
  static std::unique_ptr<Logger> Create2(const Namespace& ns, async_dispatcher_t* dispatcher,
                                         std::string_view name,
                                         FuchsiaLogSeverity min_severity = FUCHSIA_LOG_INFO,
                                         bool wait_for_initial_interest = true);
#endif

  static zx::result<std::unique_ptr<Logger>> Create(
      const Namespace& ns, async_dispatcher_t* dispatcher, std::string_view name,
      FuchsiaLogSeverity min_severity = FUCHSIA_LOG_INFO, bool wait_for_initial_interest = true)
      ZX_DEPRECATED_SINCE(1, 24, "Use Create2 which will return a no-op logger instead of failing");

  static Logger* GlobalInstance();
  static void SetGlobalInstance(Logger*);
  static bool HasGlobalInstance();

  Logger(std::string_view name, FuchsiaLogSeverity min_severity, zx::socket socket,
         fidl::WireClient<fuchsia_logger::LogSink> log_sink)
      : tag_(name),
        socket_(std::move(socket)),
        default_severity_(min_severity),
        severity_(min_severity),
        log_sink_(std::move(log_sink)) {}
  ~Logger();

  // Retrieves the number of dropped logs and resets it
  uint32_t GetAndResetDropped();

  FuchsiaLogSeverity GetSeverity();

  void SetSeverity(FuchsiaLogSeverity severity);

  void logf(FuchsiaLogSeverity severity, const char* tag, const char* file, int line,
            const char* msg, ...) __PRINTFLIKE(6, 7);
  void logvf(FuchsiaLogSeverity severity, const char* tag, const char* file, int line,
             const char* msg, va_list args);
  void logvf(FuchsiaLogSeverity severity, cpp20::span<std::string> tags, const char* file, int line,
             const char* msg, va_list args);

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD) && __cplusplus >= 202002L
  struct SeverityAndSourceLocation {
    FuchsiaLogSeverity severity;
    std::source_location loc;

    consteval SeverityAndSourceLocation(
        LogSeverity severity, const std::source_location& loc = std::source_location::current())
        : severity(severity), loc(loc) {}
  };

  template <typename... Args>
  void log(SeverityAndSourceLocation sasl, std::format_string<Args...> fmt, Args&&... args) {
    log(nullptr, sasl.severity, sasl.loc.file_name(), sasl.loc.line(), fmt,
        std::forward<Args>(args)...);
  }

  template <typename... Args>
  void log(FuchsiaLogSeverity severity, const std::source_location& loc,
           std::format_string<Args...> fmt, Args&&... args) {
    log(nullptr, severity, loc.file_name(), loc.line(), fmt, std::forward<Args>(args)...);
  }

  template <typename... Args>
  void log(const char* tag, FuchsiaLogSeverity severity, const char* file, int line,
           std::format_string<Args...> fmt, Args&&... args) {
    vlog(severity, tag, file, line, fmt.get(), std::make_format_args(args...));
  }

  void vlog(FuchsiaLogSeverity severity, const char* tag, const char* file, int line,
            std::string_view fmt, std::format_args args);
#endif

  // Begins a structured logging record. You probably don't want to call
  // this directly.
  void BeginRecord(fuchsia_syslog::LogBuffer& buffer, FuchsiaLogSeverity severity,
                   cpp17::optional<cpp17::string_view> file_name, unsigned int line,
                   cpp17::optional<cpp17::string_view> message, uint32_t dropped);

  // Sends a log record to the backend. You probably don't want to call this directly.
  // This call also increments dropped_logs_, which is why we don't call FlushRecord
  // on LogBuffer directly.
  bool FlushRecord(fuchsia_syslog::LogBuffer& buffer, uint32_t dropped);

#if FUCHSIA_API_LEVEL_AT_LEAST(24)
  bool IsNoOp() { return !socket_.is_valid(); }
#endif

 private:
  Logger(const Logger& other) = delete;
  Logger& operator=(const Logger& other) = delete;

  static zx::result<std::unique_ptr<Logger>> MaybeCreate(
      const Namespace& ns, async_dispatcher_t* dispatcher, std::string_view name,
      FuchsiaLogSeverity min_severity = FUCHSIA_LOG_INFO, bool wait_for_initial_interest = true);

  static std::unique_ptr<Logger> NoOp();

#if FUCHSIA_API_LEVEL_AT_LEAST(NEXT)
  void HandleInterest(fuchsia_diagnostics_types::wire::Interest interest);
#else
  void HandleInterest(fuchsia_diagnostics::wire::Interest interest);
#endif

  void OnInterestChange(
      fidl::WireUnownedResult<fuchsia_logger::LogSink::WaitForInterestChange>& result);

  // For thread-safety these members should be read-only.
  const std::string tag_;
  const zx::socket socket_;
  const FuchsiaLogSeverity default_severity_;
  // Messages below this won't be logged. This field is thread-safe.
  std::atomic<FuchsiaLogSeverity> severity_;
  // Dropped log count. This is thread-safe and is reset on success.
  std::atomic<uint32_t> dropped_logs_ = 0;

  // Used to learn about changes in severity.
  fidl::WireClient<fuchsia_logger::LogSink> log_sink_;
};

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD) && __cplusplus >= 202002L
// Use template type deduction to allow us to get source location while using variadic templates.
template <typename... Args>
struct trace {
  trace(std::format_string<Args...> fmt, Args&&... args,
        const std::source_location& loc = std::source_location::current()) {
    fdf::Logger::GlobalInstance()->log(FUCHSIA_LOG_TRACE, loc, fmt, std::forward<Args>(args)...);
  }
};

template <typename... Args>
trace(std::format_string<Args...>, Args&&...) -> trace<Args...>;

template <typename... Args>
struct debug {
  debug(std::format_string<Args...> fmt, Args&&... args,
        const std::source_location& loc = std::source_location::current()) {
    fdf::Logger::GlobalInstance()->log(FUCHSIA_LOG_DEBUG, loc, fmt, std::forward<Args>(args)...);
  }
};

template <typename... Args>
debug(std::format_string<Args...>, Args&&...) -> debug<Args...>;

template <typename... Args>
struct info {
  info(std::format_string<Args...> fmt, Args&&... args,
       const std::source_location& loc = std::source_location::current()) {
    fdf::Logger::GlobalInstance()->log(FUCHSIA_LOG_INFO, loc, fmt, std::forward<Args>(args)...);
  }
};

template <typename... Args>
info(std::format_string<Args...>, Args&&...) -> info<Args...>;

template <typename... Args>
struct warn {
  warn(std::format_string<Args...> fmt, Args&&... args,
       const std::source_location& loc = std::source_location::current()) {
    fdf::Logger::GlobalInstance()->log(FUCHSIA_LOG_WARNING, loc, fmt, std::forward<Args>(args)...);
  }
};

template <typename... Args>
warn(std::format_string<Args...>, Args&&...) -> warn<Args...>;

template <typename... Args>
struct error {
  error(std::format_string<Args...> fmt, Args&&... args,
        const std::source_location& loc = std::source_location::current()) {
    fdf::Logger::GlobalInstance()->log(FUCHSIA_LOG_ERROR, loc, fmt, std::forward<Args>(args)...);
  }
};

template <typename... Args>
error(std::format_string<Args...>, Args&&...) -> error<Args...>;

template <typename... Args>
struct fatal {
  fatal(std::format_string<Args...> fmt, Args&&... args,
        const std::source_location& loc = std::source_location::current()) {
    fdf::Logger::GlobalInstance()->log(FUCHSIA_LOG_ERROR, loc, fmt, std::forward<Args>(args)...);
  }
};

template <typename... Args>
fatal(std::format_string<Args...>, Args&&...) -> fatal<Args...>;

template <typename... Args>
void panic(std::format_string<Args...> fmt, Args&&... args) {
  fdf_internal::vpanic(fmt.get(), std::make_format_args(args...));
}
#endif

}  // namespace fdf

#endif  // LIB_DRIVER_LOGGING_CPP_LOGGER_H_
