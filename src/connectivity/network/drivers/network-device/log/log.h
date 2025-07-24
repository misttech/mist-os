// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_LOG_LOG_H_
#define SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_LOG_LOG_H_

#include <zircon/compiler.h>

#include <sdk/lib/syslog/cpp/log_message_impl.h>

namespace network {

void Logf(fuchsia_logging::LogSeverity severity, const char* tag, const char* file, int line,
          const char* format, ...) __PRINTFLIKE(5, 6);

}  // namespace network

#endif  // SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_LOG_LOG_H_
