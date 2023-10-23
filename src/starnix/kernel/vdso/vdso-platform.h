// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_KERNEL_VDSO_VDSO_PLATFORM_H_
#define SRC_STARNIX_KERNEL_VDSO_VDSO_PLATFORM_H_

#include <stdint.h>
#include <zircon/compiler.h>

// Defines the platform-specific implementations.

__BEGIN_CDECLS

// Returns the raw ticks value, which is obtained by reading the hardware clock (the TSC in x64).
uint64_t get_raw_ticks();

// Performs a syscall with 3 arguments.
int syscall(intptr_t syscall_number, intptr_t arg1, intptr_t arg2, intptr_t arg3);

__END_CDECLS

#endif  // SRC_STARNIX_KERNEL_VDSO_VDSO_PLATFORM_H_
