// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/syscalls/misc.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/util/cprng.h>
#include <trace.h>

#include "../kernel_priv.h"

#include <linux/random.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

using namespace starnix_uapi;

namespace starnix {

fit::result<Errno, size_t> sys_getrandom(const CurrentTask& current_task, UserAddress buf_addr,
                                         size_t size, uint32_t flags) {
  LTRACEF_LEVEL(2, "buffer 0x%lx count %zu flags 0x%x\n", buf_addr.ptr(), size, flags);

  if ((flags & !(GRND_RANDOM | GRND_NONBLOCK)) != 0) {
    return fit::error(errno(EINVAL));
  }

  auto read_result = read_to_vec<uint8_t, Errno>(
      size, [](ktl::span<uint8_t> b) -> fit::result<Errno, NumberOfElementsRead> {
        cprng_draw_uninit(b);
        return fit::ok(NumberOfElementsRead{b.size()});
      });

  if (read_result.is_error()) {
    return read_result.take_error();
  }

  ktl::span<uint8_t> buf{read_result->begin(), read_result->end()};
  auto write_result = current_task.write_memory(buf_addr, buf);
  if (write_result.is_error()) {
    return write_result.take_error();
  }

  return write_result.take_value();
}

}  // namespace starnix
