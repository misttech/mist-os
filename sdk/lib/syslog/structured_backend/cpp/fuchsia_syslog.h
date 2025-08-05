// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYSLOG_STRUCTURED_BACKEND_CPP_FUCHSIA_SYSLOG_H_
#define LIB_SYSLOG_STRUCTURED_BACKEND_CPP_FUCHSIA_SYSLOG_H_
#include <lib/syslog/structured_backend/cpp/log_buffer.h>
#include <zircon/availability.h>

#include <cstdint>

#if FUCHSIA_API_LEVEL_LESS_THAN(NEXT)
#include <lib/zx/clock.h>
#endif

namespace fuchsia_syslog {

using fuchsia_logging::FlushConfig;
using fuchsia_logging::LogBuffer;

}  // namespace fuchsia_syslog

#endif  // LIB_SYSLOG_STRUCTURED_BACKEND_CPP_FUCHSIA_SYSLOG_H_
