// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYSLOG_STRUCTURED_BACKEND_CPP_FUCHSIA_SYSLOG_H_
#define LIB_SYSLOG_STRUCTURED_BACKEND_CPP_FUCHSIA_SYSLOG_H_

#include <lib/stdcompat/optional.h>
#include <lib/stdcompat/string_view.h>
#include <lib/syslog/structured_backend/fuchsia_syslog.h>
#include <lib/zx/channel.h>
#include <lib/zx/clock.h>
#include <lib/zx/socket.h>
#include <stdint.h>

namespace fuchsia_syslog {

namespace internal {

// Opaque structure representing the backend encode state.
// This structure only has meaning to the backend and application code shouldn't
// touch these values.
// LogBuffers store the state of a log record that is in the process of being
// encoded.
// A LogBuffer is initialized by calling BeginRecord, and is written to
// the LogSink by calling FlushRecord.
// Calling BeginRecord on a LogBuffer will always initialize it to its
// clean state.
struct LogBufferData {
  // Record state (for keeping track of backend-specific details)
  uint64_t record_state[15];

  // Log data (used by the backend to encode the log into). The format
  // for this is backend-specific.
  uint64_t data[4096];
};

}  // namespace internal

// Opaque structure representing the backend encode state.
// This structure only has meaning to the backend and application code shouldn't
// touch these values.
// LogBuffers store the state of a log record that is in the process of being
// encoded.
// A LogBuffer is initialized by calling BeginRecord,
// and is written to the LogSink by calling FlushRecord.
// Calling BeginRecord on a LogBuffer will always initialize it to its
// clean state.
class LogBuffer final {
 public:
  // Initializes a LogBuffer
  //
  // buffer -- The buffer to initialize
  // severity -- The severity of the log
  // file_name -- The name of the file that generated the log message
  // line -- The line number that caused this message to be generated
  // message -- The message to output.
  // the message should be interpreted as a C-style printf before being displayed to the
  // user.
  // socket -- The socket to write the message to.
  // dropped_count -- Number of dropped messages
  // pid -- The process ID that generated the message.
  // tid -- The thread ID that generated the message.
  void BeginRecord(FuchsiaLogSeverity severity, cpp17::optional<cpp17::string_view> file_name,
                   unsigned int line, cpp17::optional<cpp17::string_view> message,
                   zx::unowned_socket socket, uint32_t dropped_count, zx_koid_t pid, zx_koid_t tid);

  // Writes a key/value pair to the buffer.
  void WriteKeyValue(cpp17::string_view key, const char* value) {
    WriteKeyValue(key, cpp17::string_view(value, strlen(value)));
  }

  void WriteKeyValue(cpp17::string_view key, cpp17::string_view value);

  // Writes a key/value pair to the buffer.
  void WriteKeyValue(cpp17::string_view key, int64_t value);

  // Writes a key/value pair to the buffer.
  void WriteKeyValue(cpp17::string_view key, uint64_t value);

  // Writes a key/value pair to the buffer.
  void WriteKeyValue(cpp17::string_view key, double value);

  // Writes a key/value pair to the buffer.
  void WriteKeyValue(cpp17::string_view key, bool value);

  // Writes the LogBuffer to the socket.
  bool FlushRecord();

 private:
  void EndRecord();

  internal::LogBufferData data_;
};

}  // namespace fuchsia_syslog

#endif  // LIB_SYSLOG_STRUCTURED_BACKEND_CPP_FUCHSIA_SYSLOG_H_
