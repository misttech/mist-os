// Copyright 2024 Mist Tecnologia LTDA
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_ARCH_MISTOS_H_
#define ZIRCON_KERNEL_INCLUDE_ARCH_MISTOS_H_

#include <sys/types.h>
#include <zircon/syscalls/debug.h>

struct Thread;

void arch_get_general_regs_mistos(Thread* thread, zx_thread_state_general_regs_t* out);
void arch_set_general_regs_mistos(Thread* thread, const zx_thread_state_general_regs_t* in);

#endif  // ZIRCON_KERNEL_INCLUDE_ARCH_MISTOS_H_
