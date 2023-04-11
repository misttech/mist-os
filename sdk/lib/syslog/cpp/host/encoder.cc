// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/syslog/cpp/host/encoder.h"

#include <inttypes.h>

#include <cstdio>

namespace syslog_backend {

const char* StripDots(const char* path) {
  while (strncmp(path, "../", 3) == 0) {
    path += 3;
  }
  return path;
}

void BeginRecordLegacy(LogBuffer* buffer, fuchsia_logging::LogSeverity severity, const char* file,
                       unsigned int line, const char* msg, const char* condition) {
  auto header = MsgHeader::CreatePtr(buffer);
  header->buffer = buffer;
  header->Init(buffer, severity);
#ifndef __Fuchsia__
  auto severity_string = GetNameForLogSeverity(severity);
  header->WriteString(severity_string.data());
  header->WriteString(": ");
#endif
  header->WriteChar('[');
  header->WriteString(StripDots(file));
  header->WriteChar('(');
  char a_buffer[128];
  snprintf(a_buffer, 128, "%i", line);
  header->WriteString(a_buffer);
  header->WriteString(")] ");
  if (condition) {
    header->WriteString("Check failed: ");
    header->WriteString(condition);
    header->WriteString(". ");
  }
  if (msg) {
    header->WriteString(msg);
    header->has_msg = true;
  }
}

// Common initialization for all KV pairs.
// Returns the header for writing the value.
MsgHeader* StartKv(LogBuffer* buffer, const char* key) {
  auto header = MsgHeader::CreatePtr(buffer);
  if (!header->first_kv || header->has_msg) {
    header->WriteChar(' ');
  }
  header->WriteString(key);
  header->WriteChar('=');
  header->first_kv = false;
  return header;
}

void WriteKeyValueLegacy(LogBuffer* buffer, const char* key, const char* value) {
  WriteKeyValueLegacy(buffer, key, value, strlen(value));
}

void WriteKeyValueLegacy(LogBuffer* buffer, const char* key, const char* value,
                         size_t value_length) {
  // "tag" has special meaning to our logging API
  if (strncmp("tag", key, value_length) == 0) {
    auto header = MsgHeader::CreatePtr(buffer);
    auto tag_size = value_length + 1;
    header->user_tag = (reinterpret_cast<char*>(buffer->data) + sizeof(buffer->data)) - tag_size;
    memcpy(header->user_tag, value, value_length);
    header->user_tag[value_length] = '\0';
    return;
  }
  auto header = StartKv(buffer, key);
  header->WriteChar('"');
  if (memchr(value, '"', value_length) != nullptr) {
    // Escape quotes in strings.
    for (size_t i = 0; i < value_length; ++i) {
      char c = value[i];
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

void WriteKeyValueLegacy(LogBuffer* buffer, const char* key, int64_t value) {
  auto header = StartKv(buffer, key);
  char a_buffer[128];
  snprintf(a_buffer, 128, "%" PRId64, value);
  header->WriteString(a_buffer);
}

void WriteKeyValueLegacy(LogBuffer* buffer, const char* key, uint64_t value) {
  auto header = StartKv(buffer, key);
  char a_buffer[128];
  snprintf(a_buffer, 128, "%" PRIu64, value);
  header->WriteString(a_buffer);
}

void WriteKeyValueLegacy(LogBuffer* buffer, const char* key, double value) {
  auto header = StartKv(buffer, key);
  char a_buffer[128];
  snprintf(a_buffer, 128, "%f", value);
  header->WriteString(a_buffer);
}

void WriteKeyValueLegacy(LogBuffer* buffer, const char* key, bool value) {
  auto header = StartKv(buffer, key);
  header->WriteString(value ? "true" : "false");
}

void EndRecordLegacy(LogBuffer* buffer) {}

}  // namespace syslog_backend
