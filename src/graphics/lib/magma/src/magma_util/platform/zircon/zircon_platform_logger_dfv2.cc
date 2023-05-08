// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon_platform_logger_dfv2.h"

#include <lib/driver/logging/cpp/logger.h>
#include <stdarg.h>
#include <stdio.h>

#include "magma_util/macros.h"
#include "platform_logger.h"

namespace magma {

fdf::Logger* g_logger;
std::string g_tag;

fit::deferred_callback InitializePlatformLoggerForDFv2(fdf::Logger* logger, std::string tag) {
  g_logger = logger;
  g_tag = std::move(tag);
  return fit::defer_callback([]() { g_logger = nullptr; });
}

void PlatformLogger::LogVa(LogLevel level, const char* file, int line, const char* fmt,
                           va_list args) {
  MAGMA_DASSERT(g_logger);
  switch (level) {
    case PlatformLogger::LOG_ERROR:
      g_logger->logvf(FUCHSIA_LOG_ERROR, g_tag.c_str(), file, line, fmt, args);
      return;
    case PlatformLogger::LOG_WARNING:
      g_logger->logvf(FUCHSIA_LOG_WARNING, g_tag.c_str(), file, line, fmt, args);
      return;
    case PlatformLogger::LOG_INFO:
      g_logger->logvf(FUCHSIA_LOG_INFO, g_tag.c_str(), file, line, fmt, args);
      return;
  }
}

}  // namespace magma
