// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_DEBUG_DEBUG_AGENT_ARCH_ZIRCON_H_
#define SRC_DEVELOPER_DEBUG_DEBUG_AGENT_ARCH_ZIRCON_H_

#include <lib/zx/thread.h>
#include <zircon/syscalls/exception.h>

// This file contains Zircon-specific interfaces that have low-levelarchitecture-dependent
// implementations.

namespace debug_agent::arch {

// Converts a Zircon exception type to a debug_ipc one. Some exception types require querying the
// thread's debug registers. If needed, the given thread will be used for that.
debug_ipc::ExceptionType DecodeExceptionType(const zx::thread& thread, uint32_t exception_type);

// Converts an architecture-specific exception record to a cross-platform one.
debug_ipc::ExceptionRecord FillExceptionRecord(const zx_exception_report_t& in);

}  // namespace debug_agent::arch

#endif  // SRC_DEVELOPER_DEBUG_DEBUG_AGENT_ARCH_ZIRCON_H_
