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
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/starnix_uapi/user_buffer.h>
#include <lib/mistos/starnix_uapi/version.h>
#include <lib/mistos/util/cprng.h>
#include <trace.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>

#include "../kernel_priv.h"

#include <linux/random.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

using namespace starnix_uapi;

namespace starnix {

fit::result<Errno> sys_uname(const CurrentTask& current_task,
                             user_out_ptr<struct new_utsname> name) {
  LTRACEF_LEVEL(2, "name 0x%p\n", name.get());
  struct new_utsname result;
  strncpy(result.sysname, "Linux", sizeof(result.sysname) / sizeof(result.sysname[0]));

  // if current_task.thread_group.read().personality.contains(PersonalityFlags::UNAME26) {
  // strncpy(result.release, "2.6.40-starnix", sizeof(result.release) / sizeof(result.release[0]));
  // } else {
  strncpy(result.release, KERNEL_RELEASE, sizeof(result.release) / sizeof(result.release[0]));
  // }

  if (auto status = current_task.write_object(
          UserRef<struct new_utsname>::New(UserAddress::from_ptr((zx_vaddr_t)(name.get()))),
          result);
      status.is_error()) {
    return status.take_error();
  }

  return fit::ok();
}

fit::result<Errno, size_t> sys_getrandom(const CurrentTask& current_task, UserAddress start_addr,
                                         size_t size, uint32_t flags) {
  LTRACEF_LEVEL(2, "buffer 0x%lx count %zu flags 0x%x\n", start_addr.ptr(), size, flags);

  if ((flags & !(GRND_RANDOM | GRND_NONBLOCK)) != 0) {
    return fit::error(errno(EINVAL));
  }

  // Copy random bytes in up-to-page-size chunks, stopping either when all the user-requested
  // space has been written to or when we fault.
  auto bytes_written = 0u;
  fbl::AllocChecker ac;
  fbl::Vector<uint8_t> bounce_buffer;
  bounce_buffer.reserve(ktl::min(static_cast<size_t>(PAGE_SIZE), size), &ac);
  if (!ac.check()) {
    return fit::error(errno(ENOMEM));
  }

  auto bytes_to_write = ktl::min(size, MAX_RW_COUNT);

  while (bytes_written < bytes_to_write) {
    auto chunk_start = start_addr.saturating_add(bytes_written);
    auto chunk_len = ktl::min(static_cast<size_t>(PAGE_SIZE), size - bytes_written);

    // Fine to index, chunk_len can't be greater than bounce_buffer.len();
    auto chunk = cprng_draw_uninit(ktl::span<uint8_t>{bounce_buffer.data(), chunk_len});
    auto result = current_task->write_memory_partial(chunk_start, chunk);
    if (result.is_ok()) {
      auto n = result.value();
      bytes_written += n;

      // If we didn't write the whole chunk then we faulted. Don't try to write any more.
      if (n < chunk_len) {
        break;
      }
    } else {
      // write_memory_partial fails if no bytes were written, but we might have
      // written bytes already.
      if (result.error_value().error_code() == EFAULT && (bytes_written > 0)) {
        break;
      }
      return result.take_error();
    }
  }

  return fit::ok(bytes_written);
}

}  // namespace starnix
