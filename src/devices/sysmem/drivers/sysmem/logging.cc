// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "logging.h"

#include <lib/ddk/debug.h>
#include <lib/driver/logging/cpp/logger.h>
#include <stdio.h>
#include <zircon/assert.h>

#include <memory>

#include <fbl/string_printf.h>

namespace sysmem_driver {

void vLog(bool is_error, const char* file, int line, const char* prefix1, const char* prefix2,
          const char* format, va_list args) {
  fbl::String new_format = fbl::StringPrintf("[%s %s] %s", prefix1, prefix2, format);
  const FuchsiaLogSeverity severity = is_error ? FUCHSIA_LOG_WARNING : FUCHSIA_LOG_DEBUG;

  fdf::Logger::GlobalInstance()->logvf(severity, nullptr, file, line, new_format.c_str(), args);
}

static std::atomic_uint64_t name_counter;

std::string CreateUniqueName(const char* prefix) {
  uint64_t new_value = name_counter++;
  return std::string(fbl::StringPrintf("%s%ld", prefix, new_value).c_str());
}
}  // namespace sysmem_driver
