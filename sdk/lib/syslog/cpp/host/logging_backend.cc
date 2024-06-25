// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <fcntl.h>
#include <inttypes.h>
#include <lib/syslog/cpp/log_level.h>
#include <lib/syslog/cpp/logging_backend.h>
#include <unistd.h>

#include <iostream>
#include <sstream>

#include "lib/syslog/cpp/host/encoder.h"

namespace syslog_runtime {

namespace {
// It's OK to keep global state here even though this file is in a source_set because on host
// we don't use shared libraries.
fuchsia_logging::LogSettings g_log_settings;

cpp17::string_view StripDots(cpp17::string_view path) {
  auto pos = path.rfind("../");
  return pos == cpp17::string_view::npos ? path : path.substr(pos + 3);
}

void BeginRecordLegacy(LogBuffer* buffer, fuchsia_logging::LogSeverity severity,
                       cpp17::optional<cpp17::string_view> file, unsigned int line,
                       cpp17::optional<cpp17::string_view> msg,
                       cpp17::optional<cpp17::string_view> condition) {
  if (!file) {
    file = "";
  }
  auto header = internal::MsgHeader::CreatePtr(buffer);
  header->buffer = buffer;
  header->Init(buffer, severity);
#ifndef __Fuchsia__
  auto severity_string = internal::GetNameForLogSeverity(severity);
  header->WriteString(severity_string.data());
  header->WriteString(": ");
#endif
  header->WriteChar('[');
  header->WriteString(StripDots(*file));
  header->WriteChar('(');
  char a_buffer[128];
  snprintf(a_buffer, 128, "%i", line);
  header->WriteString(a_buffer);
  header->WriteString(")] ");
  if (condition) {
    header->WriteString("Check failed: ");
    header->WriteString(*condition);
    header->WriteString(". ");
  }
  if (msg) {
    header->WriteString(*msg);
    header->has_msg = true;
  }
}

// Common initialization for all KV pairs.
// Returns the header for writing the value.
internal::MsgHeader* StartKv(LogBuffer* buffer, cpp17::string_view key) {
  auto header = internal::MsgHeader::CreatePtr(buffer);
  if (!header->first_kv || header->has_msg) {
    header->WriteChar(' ');
  }
  header->WriteString(key);
  header->WriteChar('=');
  header->first_kv = false;
  return header;
}

void WriteKeyValueLegacy(LogBuffer* buffer, cpp17::string_view key, cpp17::string_view value) {
  // "tag" has special meaning to our logging API
  if (key == "tag") {
    auto header = internal::MsgHeader::CreatePtr(buffer);
    auto tag_size = value.size() + 1;
    header->user_tag = (reinterpret_cast<char*>(buffer->data) + sizeof(buffer->data)) - tag_size;
    memcpy(header->user_tag, value.data(), value.size());
    header->user_tag[value.size()] = '\0';
    return;
  }
  auto header = StartKv(buffer, key);
  header->WriteChar('"');
  if (memchr(value.data(), '"', value.size()) != nullptr) {
    // Escape quotes in strings.
    for (char c : value) {
      if (c == '"') {
        header->WriteChar('\\');
      }
      header->WriteChar(c);
    }
  } else {
    header->WriteString(value);
  }
  header->WriteChar('"');
}

void WriteKeyValueLegacy(LogBuffer* buffer, cpp17::string_view key, int64_t value) {
  auto header = StartKv(buffer, key);
  char a_buffer[128];
  snprintf(a_buffer, 128, "%" PRId64, value);
  header->WriteString(a_buffer);
}

void WriteKeyValueLegacy(LogBuffer* buffer, cpp17::string_view key, uint64_t value) {
  auto header = StartKv(buffer, key);
  char a_buffer[128];
  snprintf(a_buffer, 128, "%" PRIu64, value);
  header->WriteString(a_buffer);
}

void WriteKeyValueLegacy(LogBuffer* buffer, cpp17::string_view key, double value) {
  auto header = StartKv(buffer, key);
  char a_buffer[128];
  snprintf(a_buffer, 128, "%f", value);
  header->WriteString(a_buffer);
}

void WriteKeyValueLegacy(LogBuffer* buffer, cpp17::string_view key, bool value) {
  auto header = StartKv(buffer, key);
  header->WriteString(value ? "true" : "false");
}

void EndRecordLegacy(LogBuffer* buffer) {}

}  // namespace
namespace internal {
const std::string GetNameForLogSeverity(fuchsia_logging::LogSeverity severity) {
  switch (severity) {
    case fuchsia_logging::LOG_TRACE:
      return "TRACE";
    case fuchsia_logging::LOG_DEBUG:
      return "DEBUG";
    case fuchsia_logging::LOG_INFO:
      return "INFO";
    case fuchsia_logging::LOG_WARNING:
      return "WARNING";
    case fuchsia_logging::LOG_ERROR:
      return "ERROR";
    case fuchsia_logging::LOG_FATAL:
      return "FATAL";
  }

  if (severity > fuchsia_logging::LOG_DEBUG && severity < fuchsia_logging::LOG_INFO) {
    std::ostringstream stream;
    stream << "VLOG(" << (fuchsia_logging::LOG_INFO - severity) << ")";
    return stream.str();
  }

  return "UNKNOWN";
}
}  // namespace internal
namespace {
void SetLogSettings(const fuchsia_logging::LogSettings& settings) {
  g_log_settings.min_log_level = std::min(fuchsia_logging::LOG_FATAL, settings.min_log_level);

  if (g_log_settings.log_file != settings.log_file) {
    if (!settings.log_file.empty()) {
      int fd = open(settings.log_file.c_str(), O_WRONLY | O_CREAT | O_APPEND, S_IRUSR | S_IWUSR);
      if (fd < 0) {
        std::cerr << "Could not open log file: " << settings.log_file << " (" << strerror(errno)
                  << ")" << std::endl;
      } else {
        // Redirect stderr to file.
        if (dup2(fd, STDERR_FILENO) < 0) {
          std::cerr << "Could not set stderr to log file: " << settings.log_file << " ("
                    << strerror(errno) << ")" << std::endl;
        } else {
          g_log_settings.log_file = settings.log_file;
        }
        close(fd);
      }
    }
  }
}

void SetLogSettings(const fuchsia_logging::LogSettings& settings,
                    const std::initializer_list<std::string>& tags) {
  syslog_runtime::SetLogSettings(settings);
}
}  // namespace
void SetLogTags(const std::initializer_list<std::string>& tags) {
  // Global tags aren't supported on host.
}

fuchsia_logging::LogSeverity GetMinLogSeverity() {
  return syslog_runtime::g_log_settings.min_log_level;
}

void BeginRecord(LogBuffer* buffer, fuchsia_logging::LogSeverity severity, NullSafeStringView file,
                 unsigned int line, NullSafeStringView msg, NullSafeStringView condition) {
  BeginRecordLegacy(buffer, severity, file, line, msg, condition);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, cpp17::string_view value) {
  WriteKeyValueLegacy(this, key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, int64_t value) {
  WriteKeyValueLegacy(this, key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, uint64_t value) {
  WriteKeyValueLegacy(this, key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, double value) {
  WriteKeyValueLegacy(this, key, value);
}

void LogBuffer::WriteKeyValue(cpp17::string_view key, bool value) {
  WriteKeyValueLegacy(this, key, value);
}

void EndRecord(LogBuffer* buffer) { EndRecordLegacy(buffer); }

bool LogBuffer::Flush() {
  auto header = internal::MsgHeader::CreatePtr(this);
  *(header->offset++) = 0;
  if (header->user_tag) {
    auto tag = header->user_tag;
    std::cerr << "[" << tag << "] ";
  }
  std::cerr << reinterpret_cast<const char*>(this->data) << std::endl;
  return true;
}

void WriteLog(fuchsia_logging::LogSeverity severity, const char* file, unsigned int line,
              const char* tag, const char* condition, const std::string& msg) {
  if (tag)
    std::cerr << "[" << tag << "] ";

  std::cerr << "[" << internal::GetNameForLogSeverity(severity) << ":" << file << "(" << line
            << ")]";

  if (condition)
    std::cerr << " Check failed: " << condition << ".";

  std::cerr << msg << std::endl;
  std::cerr.flush();
}

LogBuffer LogBufferBuilder::Build() {
  LogBuffer buffer;
  BeginRecord(&buffer, severity_, NullSafeStringView::CreateFromOptional(file_name_), line_,
              NullSafeStringView::CreateFromOptional(msg_),
              NullSafeStringView::CreateFromOptional(condition_));
  return buffer;
}
}  // namespace syslog_runtime

namespace fuchsia_logging {

// Sets the default log severity. If not explicitly set,
// this defaults to INFO, or to the value specified by Archivist.
LogSettingsBuilder& LogSettingsBuilder::WithMinLogSeverity(LogSeverity min_log_level) {
  settings_.min_log_level = min_log_level;
  return *this;
}

fuchsia_logging::LogSeverity GetMinLogSeverity() {
  return syslog_runtime::g_log_settings.min_log_level;
}

// Sets the log file.
LogSettingsBuilder& LogSettingsBuilder::WithLogFile(const std::string_view& log_file) {
  settings_.log_file = log_file;
  return *this;
}

// Configures the log settings with the specified tags.
void LogSettingsBuilder::BuildAndInitializeWithTags(
    const std::initializer_list<std::string>& tags) {
  syslog_runtime::SetLogSettings(settings_, tags);
}

// Configures the log settings.
void LogSettingsBuilder::BuildAndInitialize() { syslog_runtime::SetLogSettings(settings_); }

}  // namespace fuchsia_logging
