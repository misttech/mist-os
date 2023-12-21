// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_COMPONENTS_CPP_LOG_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_COMPONENTS_CPP_LOG_H_

#include <lib/syslog/structured_backend/cpp/fuchsia_syslog.h>
#include <zircon/compiler.h>

// The logging code here has been introduced to enable using this component for
// both DFv1 and DFv2 drivers.
// TODO(b/317249253) This should be removed once DFv1 is deprecated
// (or all fullmac drivers have been ported over to DFv2).
namespace wlan::drivers::components::internal {

enum class LogMode { DFv1, DFv2 };

void set_log_mode(LogMode mode);

void logf(FuchsiaLogSeverity severity, const char* file, int line, const char* msg, ...)
    __PRINTFLIKE(4, 5);

#define LOGF(flag, msg...) \
  wlan::drivers::components::internal::logf(FUCHSIA_LOG_##flag, __FILE__, __LINE__, msg)

}  // namespace wlan::drivers::components::internal

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_COMPONENTS_CPP_LOG_H_
