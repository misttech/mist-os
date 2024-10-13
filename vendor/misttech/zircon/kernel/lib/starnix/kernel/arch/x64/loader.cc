// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/loader.h"

#include <lib/mistos/starnix/kernel/arch/x64/registers.h>

namespace starnix {

::zx_thread_state_general_regs_t zx_thread_state_general_regs_t_from(const ThreadStartInfo& val) {
  ::zx_thread_state_general_regs_t result;
  result.rip = reinterpret_cast<uint64_t>(val.entry.ptr());
  result.rsp = reinterpret_cast<uint64_t>(val.stack.ptr());
  // You can initialize other fields here if necessary
  return result;
}

}  // namespace starnix
