// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYSLOG_CPP_LOGGING_BACKEND_FUCHSIA_GLOBALS_H_
#define LIB_SYSLOG_CPP_LOGGING_BACKEND_FUCHSIA_GLOBALS_H_

#include <zircon/types.h>

#include <cstdint>

namespace syslog_runtime {
class LogState;
}  // namespace syslog_runtime

extern "C" {

void FuchsiaLogAcquireState();

void FuchsiaLogSetStateLocked(syslog_runtime::LogState* new_state);

void FuchsiaLogReleaseState();

syslog_runtime::LogState* FuchsiaLogGetStateLocked();

uint32_t FuchsiaLogGetAndResetDropped();

void FuchsiaLogAddDropped(uint32_t count);

zx_koid_t FuchsiaLogGetCurrentThreadKoid();

}  // extern "C"

#endif  // LIB_SYSLOG_CPP_LOGGING_BACKEND_FUCHSIA_GLOBALS_H_
