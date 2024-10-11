// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/signals/syscalls.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/starnix_uapi/user_buffer.h>

#include "../kernel_priv.h"

#include <linux/random.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

using namespace starnix_uapi;

namespace starnix {

fit::result<Errno, pid_t> sys_wait4(const CurrentTask& current_task, pid_t raw_selector,
                                    starnix_uapi::UserRef<int32_t> user_wstatus, uint32_t options,
                                    starnix_uapi::UserRef<struct ::rusage> user_rusage) {
  return fit::error(errno(ENOSYS));
}

}  // namespace starnix
