// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch/crashlog.h>
#include <stdio.h>
#include <inttypes.h>

void arch_render_crashlog_registers(FILE& target, const crashlog_regs_t& regs) {
  fprintf(&target, "TODO: print RISCV registers here\n");
}

