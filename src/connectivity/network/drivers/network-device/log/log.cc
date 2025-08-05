// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/network/drivers/network-device/log/log.h"

#include <cstdarg>

namespace network {

void Logf(fuchsia_logging::LogSeverity severity, const char* tag, const char* file, int line,
          const char* format, ...) {
  va_list args;
  va_start(args, format);
  char buffer[1024];
  vsnprintf(buffer, sizeof(buffer), format, args);
  fuchsia_logging::LogMessage(severity, file, line, nullptr, tag).stream() << buffer;
}

}  // namespace network
