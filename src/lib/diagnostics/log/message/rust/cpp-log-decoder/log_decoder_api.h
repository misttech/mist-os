// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_DIAGNOSTICS_LOG_MESSAGE_RUST_CPP_LOG_DECODER_LOG_DECODER_API_H_
#define SRC_LIB_DIAGNOSTICS_LOG_MESSAGE_RUST_CPP_LOG_DECODER_LOG_DECODER_API_H_
#include "log_decoder.h"

namespace log_tester {

// Creates a C++ string from a Rust string
std::string StringFromRustString(CPPArray<uint8_t> rust_string) {
  std::string ret;
  if (rust_string.ptr) {
    ret = std::string(reinterpret_cast<const char*>(rust_string.ptr), rust_string.len);
  }
  return ret;
}

inline fpromise::result<fuchsia::logger::LogMessage, std::string> ToFidlLogMessage(
    LogMessage* message) {
  std::vector<std::string> tags;
  for (size_t i = 0; i < message->tags.len; i++) {
    tags.push_back(StringFromRustString(message->tags.ptr[i]));
  }
  fuchsia::logger::LogMessage ret = {
      .pid = message->pid,
      .tid = message->tid,
      .time = zx::time_boot(message->timestamp),
      .severity = message->severity,
      .dropped_logs = static_cast<uint32_t>(message->dropped),
      .tags = std::move(tags),
      .msg = StringFromRustString(message->message),
  };
  return fpromise::ok(std::move(ret));
}
}  // namespace log_tester

#endif  // SRC_LIB_DIAGNOSTICS_LOG_MESSAGE_RUST_CPP_LOG_DECODER_LOG_DECODER_API_H_
