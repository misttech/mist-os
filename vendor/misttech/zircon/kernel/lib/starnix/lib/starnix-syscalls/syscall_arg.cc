// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix_syscalls/syscall_arg.h"

namespace starnix_syscalls {

template <>
bool from<bool>(SyscallArg arg) {
  return arg.raw() != 0;
}

}  // namespace starnix_syscalls
