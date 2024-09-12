// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "logging.h"

#include <lib/syslog/cpp/macros.h>
#include <stdio.h>
#include <zircon/assert.h>

#include <memory>

#include <fbl/string_printf.h>

namespace sysmem_service {

void vLog(::fuchsia_logging::LogSeverity severity, const char* file, int line, const char* prefix,
          const char* format, va_list args) {
  fbl::String new_format;
  if (prefix) {
    new_format = fbl::StringPrintf("[%s] %s", prefix, format);
  } else {
    new_format = fbl::StringPrintf("%s", format);
  }
  fbl::String formatted = fbl::StringVPrintf(new_format.c_str(), args);
  const char* formatted_str = formatted.c_str();
  ::fuchsia_logging::LogMessage(severity, file, line, nullptr, nullptr).stream() << formatted_str;
}

void Log(::fuchsia_logging::LogSeverity severity, const char* file, int line, const char* prefix,
         const char* format, ...) {
  va_list args;
  va_start(args, format);
  vLog(severity, file, line, prefix, format, args);
  va_end(args);
}

static std::atomic_uint64_t name_counter;

std::string CreateUniqueName(const char* prefix) {
  uint64_t new_value = name_counter++;
  return std::string(fbl::StringPrintf("%s%ld", prefix, new_value).c_str());
}

}  // namespace sysmem_service
