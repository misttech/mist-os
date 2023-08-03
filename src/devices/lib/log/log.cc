// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "log.h"

#include <fidl/fuchsia.logger/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <zircon/process.h>

#include "fidl/fuchsia.logger/cpp/wire_types.h"

namespace {
const char* StripPath(const char* path) {
  auto p = strrchr(path, '/');
  if (p) {
    return p + 1;
  }
  return path;
}

const char* StripDots(const char* path) {
  while (strncmp(path, "../", 3) == 0) {
    path += 3;
  }
  return path;
}

const char* StripFile(const char* file, FuchsiaLogSeverity severity) {
  return severity > FUCHSIA_LOG_INFO ? StripDots(file) : StripPath(file);
}

}  // namespace

namespace driver_logger {
namespace internal {
FuchsiaLogSeverity severity_from_verbosity(uint8_t verbosity) {
  // verbosity scale sits in the interstitial space between INFO and DEBUG
  FuchsiaLogSeverity severity = FUCHSIA_LOG_INFO - (verbosity * FUCHSIA_LOG_VERBOSITY_STEP_SIZE);
  if (severity < FUCHSIA_LOG_DEBUG + 1) {
    return FUCHSIA_LOG_DEBUG + 1;
  }
  return severity;
}

void log_with_source(Logger& logger, FuchsiaLogSeverity severity, const char* tag, const char* file,
                     int line, const char* format, ...) {
  va_list args;
  va_start(args, format);
  logger.VLogWrite(severity, tag, format, args, file, line);
  va_end(args);
}
}  // namespace internal

void Logger::VLogWrite(FuchsiaLogSeverity severity, const char* tag, const char* msg, va_list args,
                       const char* file, uint32_t line) const {
  fuchsia_syslog::LogBuffer buffer;
  constexpr size_t kFormatStringLength = 1024;
  char fmt_string[kFormatStringLength];
  fmt_string[kFormatStringLength - 1] = 0;
  int n = kFormatStringLength;
  // Format
  // Number of bytes written not including null terminator
  int count = 0;
  count = vsnprintf(fmt_string, n, msg, args) + 1;

  if (count >= n) {
    // truncated
    constexpr char kEllipsis[] = "...";
    constexpr size_t kEllipsisSize = sizeof(kEllipsis);
    snprintf(fmt_string + kFormatStringLength - 1 - kEllipsisSize, kEllipsisSize, kEllipsis);
  }

  if (file) {
    file = StripFile(file, severity);
  }
  BeginRecord(buffer, severity, file, line, fmt_string);
  for (const auto& tag : tags_) {
    buffer.WriteKeyValue("tag", tag);
  }
  if (tag) {
    buffer.WriteKeyValue("tag", tag);
  }
  FlushRecord(buffer, severity);
}

void Logger::BeginRecord(fuchsia_syslog::LogBuffer& buffer, FuchsiaLogSeverity severity,
                         cpp17::optional<cpp17::string_view> file_name, unsigned int line,
                         cpp17::optional<cpp17::string_view> msg) const {
  buffer.BeginRecord(severity, file_name, line, msg, socket_.borrow(), 0, pid_, GetCurrentThread());
}

void Logger::FlushRecord(fuchsia_syslog::LogBuffer& buffer, FuchsiaLogSeverity severity) const {
  if (severity < severity_) {
    return;
  }
  buffer.FlushRecord();
}

static zx_koid_t GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

static zx_koid_t pid = GetKoid(zx_process_self());

static zx_koid_t GetCurrentThreadKoid() { return GetKoid(zx_thread_self()); }

static thread_local zx_koid_t tid = GetCurrentThreadKoid();

zx_koid_t GetCurrentThread() { return tid; }

zx::result<Logger> CreateLogger() {
  zx::channel logger_request, logger_client;
  zx::channel::create(0, &logger_request, &logger_client);
  fdio_service_connect("/svc/fuchsia.logger.LogSink", logger_request.release());
  zx::socket remote, local;
  zx::socket::create(ZX_SOCKET_DATAGRAM, &remote, &local);
  fidl::ClientEnd<fuchsia_logger::LogSink> log_sink{std::move(logger_client)};
  auto status = fidl::WireCall(log_sink)->ConnectStructured(std::move(remote));
  if (!status.ok()) {
    return zx::error(status.error().status());
  }
  return zx::ok(Logger(pid, std::move(local)));
}

struct GlobalLogger {
  std::optional<Logger> logger;

 public:
  GlobalLogger() { logger = *CreateLogger(); }
};
static GlobalLogger instance;

Logger& GetLogger() { return *instance.logger; }
}  // namespace driver_logger
