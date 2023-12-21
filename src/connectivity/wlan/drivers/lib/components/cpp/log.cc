// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/connectivity/wlan/drivers/lib/components/cpp/log.h"

#include <lib/ddk/debug.h>
#include <stdarg.h>

#include <sdk/lib/driver/logging/cpp/logger.h>

// Since this code has references to the ddk which is not included
// for DFv2 drivers, include this export to prevent linker errors.
// TODO(b/317249253) This should be removed once DFv1 is deprecated
// (or all fullmac drivers have been ported over to DFv2).
__EXPORT
__WEAK zx_driver_rec __zircon_driver_rec__ = {
    .ops = {},
    .driver = {},
};
namespace wlan::drivers::components::internal {

LogMode gLogMode = LogMode::DFv1;

void set_log_mode(LogMode mode) { gLogMode = mode; }

void logf(FuchsiaLogSeverity severity, const char* file, int line, const char* msg, ...) {
  va_list args;
  va_start(args, msg);
  switch (gLogMode) {
    case LogMode::DFv1:
      zxlogvf_etc(severity, nullptr, file, line, msg, args);
      break;
    case LogMode::DFv2:
      fdf::Logger::GlobalInstance()->logvf(severity, nullptr, file, line, msg, args);
      break;
  }
  va_end(args);
}

}  // namespace wlan::drivers::components::internal
