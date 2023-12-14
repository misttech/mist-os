// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/syslog/cpp/host/encoder.h"

#include <inttypes.h>

#include <cstdio>

namespace syslog_runtime {

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
  auto header = MsgHeader::CreatePtr(buffer);
  header->buffer = buffer;
  header->Init(buffer, severity);
#ifndef __Fuchsia__
  auto severity_string = GetNameForLogSeverity(severity);
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
MsgHeader* StartKv(LogBuffer* buffer, cpp17::string_view key) {
  auto header = MsgHeader::CreatePtr(buffer);
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
    auto header = MsgHeader::CreatePtr(buffer);
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

}  // namespace syslog_runtime
