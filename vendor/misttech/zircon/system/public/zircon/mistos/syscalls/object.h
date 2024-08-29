// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_MISTOS_STACK_H_
#define ZIRCON_MISTOS_STACK_H_

#include <zircon/types.h>

__BEGIN_CDECLS

// ask clang format not to mess up the indentation:
// clang-format off

#define ZX_PROP_MISTOS_OFFSET               100u

#define ZX_PROP_MISTOS_PROCESS_STACK        ZX_PROP_MISTOS_OFFSET + 1

typedef struct zx_mistos_process_stack {
  zx_vaddr_t stack_pointer;
  zx_vaddr_t auxv_start;
  zx_vaddr_t auxv_end;
  zx_vaddr_t argv_start;
  zx_vaddr_t argv_end;
  zx_vaddr_t environ_start;
  zx_vaddr_t environ_end;
} zx_mistos_process_stack_t;

__END_CDECLS

#endif  // ZIRCON_MISTOS_STACK_H_
