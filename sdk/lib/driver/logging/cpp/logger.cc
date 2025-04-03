// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.logger/cpp/wire_messaging.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fdio/directory.h>
#include <zircon/process.h>

#include <cstdarg>

namespace flog = fuchsia_syslog;

namespace fdf {

namespace {
std::atomic<Logger*> g_instance = nullptr;

#if FUCHSIA_API_LEVEL_AT_LEAST(NEXT)
using FidlSeverity = fuchsia_diagnostics_types::wire::Severity;
using FidlInterest = fuchsia_diagnostics_types::wire::Interest;
#else
using FidlSeverity = fuchsia_diagnostics::wire::Severity;
using FidlInterest = fuchsia_diagnostics::wire::Interest;
#endif

}  // namespace

zx_koid_t GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

bool Logger::FlushRecord(flog::LogBuffer& buffer, uint32_t dropped) {
  if (!buffer.FlushRecord()) {
    dropped_logs_.fetch_add(dropped, std::memory_order_relaxed);
    return false;
  }
  return true;
}

void Logger::BeginRecord(flog::LogBuffer& buffer, FuchsiaLogSeverity severity,
                         cpp17::optional<cpp17::string_view> file_name, unsigned int line,
                         cpp17::optional<cpp17::string_view> message, uint32_t dropped) {
  static zx_koid_t pid = GetKoid(zx_process_self());
  static thread_local zx_koid_t tid = GetKoid(zx_thread_self());
  buffer.BeginRecord(severity, file_name, line, message, socket_.borrow(), dropped, pid, tid);
  buffer.WriteKeyValue("tag", "driver");
  buffer.WriteKeyValue("tag", tag_);
}

std::unique_ptr<Logger> Logger::NoOp() {
  fidl::WireClient<fuchsia_logger::LogSink> log_sink;
  return std::make_unique<Logger>("", FUCHSIA_LOG_INFO, zx::socket(), std::move(log_sink));
}

#if FUCHSIA_API_LEVEL_AT_LEAST(24)
std::unique_ptr<Logger> Logger::Create2(const Namespace& ns, async_dispatcher_t* dispatcher,
                                        std::string_view name, FuchsiaLogSeverity min_severity,
                                        bool wait_for_initial_interest) {
  auto result = Logger::MaybeCreate(ns, dispatcher, name, min_severity, wait_for_initial_interest);
  if (!result.is_ok()) {
    return Logger::NoOp();
  }
  return std::move(result.value());
}
#endif

zx::result<std::unique_ptr<Logger>> Logger::Create(const Namespace& ns,
                                                   async_dispatcher_t* dispatcher,
                                                   std::string_view name,
                                                   FuchsiaLogSeverity min_severity,
                                                   bool wait_for_initial_interest) {
  auto result = Logger::MaybeCreate(ns, dispatcher, name, min_severity, wait_for_initial_interest);
  if (!result.is_ok()) {
    return zx::ok(Logger::NoOp());
  }
  return result;
}

zx::result<std::unique_ptr<Logger>> Logger::MaybeCreate(const Namespace& ns,
                                                        async_dispatcher_t* dispatcher,
                                                        std::string_view name,
                                                        FuchsiaLogSeverity min_severity,
                                                        bool wait_for_initial_interest) {
  zx::socket client_end, server_end;
  zx_status_t status = zx::socket::create(ZX_SOCKET_DATAGRAM, &client_end, &server_end);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  auto ns_result = ns.Connect<fuchsia_logger::LogSink>();
  if (ns_result.is_error()) {
    return ns_result.take_error();
  }

  fidl::WireClient<fuchsia_logger::LogSink> log_sink(std::move(*ns_result), dispatcher);
  auto sink_result = log_sink->ConnectStructured(std::move(server_end));
  if (!sink_result.ok()) {
    return zx::error(sink_result.status());
  }

  auto logger =
      std::make_unique<Logger>(name, min_severity, std::move(client_end), std::move(log_sink));

  if (wait_for_initial_interest) {
    auto interest_result = logger->log_sink_.sync()->WaitForInterestChange();
    if (!interest_result.ok()) {
      return zx::error(interest_result.status());
    }
    // We are guanteed to not call this twice to we can ignore the application error.
    logger->HandleInterest(interest_result->value()->data);
  }

  logger->log_sink_->WaitForInterestChange().Then(
      fit::bind_member(logger.get(), &Logger::OnInterestChange));

  return zx::ok(std::move(logger));
}

Logger* Logger::GlobalInstance() {
  ZX_DEBUG_ASSERT(HasGlobalInstance());
  return g_instance.load();
}

void Logger::SetGlobalInstance(Logger* logger) { g_instance = logger; }

bool Logger::HasGlobalInstance() { return g_instance != nullptr; }

Logger::~Logger() = default;

void Logger::HandleInterest(FidlInterest interest) {
  if (interest.has_min_severity()) {
    switch (interest.min_severity()) {
      case FidlSeverity::kTrace:
        severity_ = FUCHSIA_LOG_TRACE;
        return;
      case FidlSeverity::kDebug:
        severity_ = FUCHSIA_LOG_DEBUG;
        return;
      case FidlSeverity::kInfo:
        severity_ = FUCHSIA_LOG_INFO;
        return;
      case FidlSeverity::kWarn:
        severity_ = FUCHSIA_LOG_WARNING;
        return;
      case FidlSeverity::kError:
        severity_ = FUCHSIA_LOG_ERROR;
        return;
      case FidlSeverity::kFatal:
        severity_ = FUCHSIA_LOG_FATAL;
        return;
#if FUCHSIA_API_LEVEL_AT_LEAST(NEXT)
      default:
        severity_ = FUCHSIA_LOG_INFO;
        return;
#endif
    }
  } else {
    severity_ = default_severity_;
  }
}

void Logger::OnInterestChange(
    fidl::WireUnownedResult<fuchsia_logger::LogSink::WaitForInterestChange>& result) {
  if (result.ok()) {
    HandleInterest(result->value()->data);
    log_sink_->WaitForInterestChange().Then(fit::bind_member(this, &Logger::OnInterestChange));
  }
}

uint32_t Logger::GetAndResetDropped() {
  return dropped_logs_.exchange(0, std::memory_order_relaxed);
}

FuchsiaLogSeverity Logger::GetSeverity() { return severity_.load(std::memory_order_relaxed); }

void Logger::SetSeverity(FuchsiaLogSeverity severity) { severity_.store(severity); }

void Logger::logf(FuchsiaLogSeverity severity, const char* tag, const char* file, int line,
                  const char* msg, ...) {
  va_list args;
  va_start(args, msg);
  logvf(severity, tag, file, line, msg, args);
  va_end(args);
}

namespace {
const char* StripDots(const char* path) {
  while (strncmp(path, "../", 3) == 0) {
    path += 3;
  }
  return path;
}

const char* StripPath(const char* path) {
  auto p = strrchr(path, '/');
  if (p) {
    return p + 1;
  } else {
    return path;
  }
}
}  // namespace

static const char* StripFile(const char* file, FuchsiaLogSeverity severity) {
  return severity > FUCHSIA_LOG_INFO ? StripDots(file) : StripPath(file);
}

void Logger::logvf(FuchsiaLogSeverity severity, const char* tag, const char* file, int line,
                   const char* msg, va_list args) {
  if (tag) {
    std::string tag_str(tag);
    logvf(severity, {&tag_str, 1}, file, line, msg, args);
  } else {
    logvf(severity, cpp20::span<std::string>(), file, line, msg, args);
  }
}

void Logger::logvf(FuchsiaLogSeverity severity, cpp20::span<std::string> tags, const char* file,
                   int line, const char* msg, va_list args) {
  if (!file || line <= 0) {
    // We require a file and line number for printf-style logs.
    return;
  }
  if (severity < severity_.load()) {
    return;
  }
  uint32_t dropped = dropped_logs_.exchange(0, std::memory_order_relaxed);
  constexpr size_t kFormatStringLength = 1024;
  char fmt_string[kFormatStringLength];
  fmt_string[kFormatStringLength - 1] = 0;
  int n = kFormatStringLength;
  // Format
  // Number of bytes written not including null terminator
  int count = 0;
  count = vsnprintf(fmt_string, n, msg, args) + 1;
  if (count < 0) {
    // Invalid arguments -- we don't support logging empty strings
    // for legacy printf-style messages.
    return;
  }

  if (count >= n) {
    // truncated
    constexpr char kEllipsis[] = "...";
    constexpr size_t kEllipsisSize = sizeof(kEllipsis);
    snprintf(fmt_string + kFormatStringLength - 1 - kEllipsisSize, kEllipsisSize, kEllipsis);
  }

  file = StripFile(file, severity);
  flog::LogBuffer buffer;
  BeginRecord(buffer, severity, file, line, fmt_string, dropped);
  for (const auto& tag : tags) {
    buffer.WriteKeyValue("tag", tag);
  }
  FlushRecord(buffer, dropped);

  if (severity == FUCHSIA_LOG_FATAL) {
    abort();
  }
}

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD) && __cplusplus >= 202002L
namespace {
template <typename T, std::size_t N>
class array_output_iterator {
 public:
  using iterator_category = std::output_iterator_tag;
  using value_type = T;
  using difference_type = std::ptrdiff_t;
  using pointer = T*;
  using reference = T&;

  explicit array_output_iterator(std::array<T, N>& arr, size_t& actual_size)
      : arr_(arr), actual_size_(actual_size) {}

  array_output_iterator(array_output_iterator&& other)
      : arr_(other.arr_), actual_size_(other.actual_size_), index_(other.index_) {}
  array_output_iterator& operator=(array_output_iterator&& other) {
    arr_ = other.arr_;
    actual_size_ = other.actual_size_;
    index_ = other.index_;
    return *this;
  }

  array_output_iterator& operator=(const T& value) {
    if (index_ < N) {
      arr_[index_] = value;
    }
    return *this;
  }

  reference operator*() { return arr_[index_]; }
  array_output_iterator& operator++() {
    ++index_;
    actual_size_++;
    return *this;
  }
  array_output_iterator operator++(int) {
    auto tmp = *this;
    ++index_;
    actual_size_++;
    return tmp;
  }

 private:
  std::array<T, N>& arr_;
  size_t& actual_size_;
  size_t index_ = 0;
};

template <typename T, size_t N>
array_output_iterator(std::array<T, N>&, size_t&) -> array_output_iterator<T, N>;
}  // namespace

void Logger::vlog(FuchsiaLogSeverity severity, const char* tag, const char* file, int line,
                  std::string_view fmt, std::format_args args) {
  if (severity < severity_.load()) {
    return;
  }
  constexpr size_t kFormatStringLength = 1024;
  std::array<char, kFormatStringLength> fmt_buffer;
  size_t actual_size = 0;

  std::vformat_to(array_output_iterator(fmt_buffer, actual_size), fmt, args);
  if (actual_size == 0) {
    return;
  }

  uint32_t dropped = dropped_logs_.exchange(0, std::memory_order_relaxed);

  if (actual_size >= kFormatStringLength) {
    // truncated
    constexpr char kEllipsis[] = "...";
    constexpr size_t kEllipsisSize = sizeof(kEllipsis);
    snprintf(fmt_buffer.data() + kFormatStringLength - kEllipsisSize, kEllipsisSize, kEllipsis);
  }
  fmt_buffer[kFormatStringLength - 1] = 0;

  file = StripFile(file, severity);
  flog::LogBuffer buffer;
  BeginRecord(buffer, severity, file, line,
              std::string_view(fmt_buffer.data(), std::min(actual_size, kFormatStringLength)),
              dropped);
  if (tag) {
    buffer.WriteKeyValue("tag", tag);
  }
  FlushRecord(buffer, dropped);

  if (severity == FUCHSIA_LOG_FATAL) {
    abort();
  }
}
#endif

}  // namespace fdf
