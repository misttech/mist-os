// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/mm/syscalls.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/bitflags.h>
#include <zircon/compiler.h>

#include <linux/mman.h>

namespace starnix {

using namespace starnix_uapi;

#ifdef __x86_64__
// Returns any platform-specific mmap flags
uint32_t get_valid_platform_mmap_flags() { return MAP_32BIT; }
#else
uint32_t get_valid_platform_mmap_flags() { return 0; }
#endif

fit::result<Errno, UserAddress> sys_mmap(const CurrentTask& current_task, UserAddress addr,
                                         size_t length, uint32_t prot, uint32_t flags, FdNumber fd,
                                         uint64_t offset) {
  auto user_address = do_mmap(current_task, addr, length, prot, flags, fd, offset);
  if (user_address.is_error()) {
    return user_address.take_error();
  }

  if ((prot & PROT_EXEC) != 0) {
    // Possibly loads a new module. Notify debugger for the change.
    // We only care about dynamic linker loading modules for now, which uses mmap. In the future
    // we might want to support unloading modules in munmap or JIT compilation in mprotect.

    // notify_debugger_of_module_list(current_task)?;
  }

  return user_address;
}

fit::result<Errno, UserAddress> do_mmap(const CurrentTask& current_task, UserAddress addr,
                                        size_t length, uint32_t prot, uint32_t flags, FdNumber fd,
                                        uint64_t offset) {
  auto prot_flags = ProtectionFlags::from_bits(prot);
  if (!prot_flags) {
    // track_stub!(TODO("https://fxbug.dev/322874211"), "mmap parse protection", prot);
    return fit::error(errno(EINVAL));
  }

  uint32_t valid_flags = get_valid_platform_mmap_flags() | MAP_PRIVATE | MAP_SHARED |
                         MAP_SHARED_VALIDATE | MAP_ANONYMOUS | MAP_FIXED | MAP_FIXED_NOREPLACE |
                         MAP_POPULATE | MAP_NORESERVE | MAP_STACK | MAP_DENYWRITE | MAP_GROWSDOWN;

  if ((flags & !valid_flags) != 0) {
    if ((flags & MAP_SHARED_VALIDATE) != 0) {
      return fit::error(errno(EOPNOTSUPP));
    }
    // track_stub !(TODO("https://fxbug.dev/322873638"), "mmap check flags", flags);
    return fit::error(errno(EINVAL));
  }

  // let file = if flags & MAP_ANONYMOUS != 0 { None } else { Some(current_task.files.get(fd)?) };
  if ((flags & (MAP_PRIVATE | MAP_SHARED)) == 0 ||
      (flags & (MAP_PRIVATE | MAP_SHARED)) == (MAP_PRIVATE | MAP_SHARED)) {
    return fit::error(errno(EINVAL));
  }

  if (length == 0) {
    return fit::error(errno(EINVAL));
  }
  if (offset % PAGE_SIZE != 0) {
    return fit::error(errno(EINVAL));
  }

  // TODO(tbodt): should we consider MAP_NORESERVE?

  auto cond1 = (flags & MAP_FIXED) != 0;
  auto cond2 = (flags & MAP_FIXED_NOREPLACE) != 0;
  DesiredAddress daddr;
  if (addr == 0) {
    if (cond1 == false && cond2 == false) {
      daddr = {.type=DesiredAddressType::Any, .address=0};
    } else if (cond1 == true || cond2 == true) {
      return fit::error(errno(EINVAL));
    }
  } else {
    if (cond1 == false && cond2 == false) {
      daddr = {.type=DesiredAddressType::Hint, .address=addr};
    } else if (cond2) {
      daddr = {.type=DesiredAddressType::Fixed, .address=addr};
    } else if (cond1 == true && cond2 == false) {
      daddr = {.type=DesiredAddressType::FixedOverwrite, .address=addr};
    }
  }

  // uint64_t memory_offset = (flags & MAP_ANONYMOUS) ? 0 : offset;

  auto options = MappingOptionsFlags::empty();
  if (flags & MAP_SHARED) {
    options |= MappingOptions::SHARED;
  }

  if (flags & MAP_ANONYMOUS) {
    options |= MappingOptions::ANONYMOUS;
  }
#ifdef __x86_64__
  if (!(flags & MAP_FIXED) && (flags & MAP_32BIT)) {
    options |= MappingOptions::LOWER_32BIT;
  }
#endif
  if (flags & MAP_GROWSDOWN) {
    options |= MappingOptions::GROWSDOWN;
  }
  if (flags & MAP_POPULATE) {
    options |= MappingOptions::POPULATE;
  }

  if ((flags & MAP_ANONYMOUS) != 0) {
    return current_task->mm()->map_anonymous(daddr, length, prot_flags.value(), options,
                                             {.type=MappingNameType::None});
  } else {
    /*
    // TODO(tbodt): maximize protection flags so that mprotect works
    let file = current_task.files.get(fd)?;
    file.mmap(current_task, addr, memory_offset, length, prot_flags, options, file.name.clone())
    */
  }

  return fit::ok(0);
}

fit::result<Errno> sys_munmap(const CurrentTask& current_task, UserAddress addr, size_t length) {
  auto result = current_task->mm()->unmap(addr, length);
  if (result.is_error()) {
    return result.take_error();
  }
  return fit::ok();
}

fit::result<Errno> sys_mprotect(const CurrentTask& current_task, UserAddress addr, size_t length,
                                uint32_t prot) {
  auto prot_flags = ProtectionFlags::from_bits(prot);
  if (!prot_flags.has_value()) {
    // track_stub !(TODO("https://fxbug.dev/322874672"), "mprotect parse protection", prot);
    return fit::error(errno(EINVAL));
  }
  auto result = current_task->mm()->protect(addr, length, prot_flags.value());
  if (result.is_error()) {
    return result.take_error();
  }
  return fit::ok();
}

fit::result<Errno, UserAddress> sys_mremap(const CurrentTask& current_task, UserAddress addr,
                                           size_t old_length, size_t new_length, uint32_t flags,
                                           UserAddress new_addr) {
  return fit::error(errno(ENOSYS));
}

/*

pub fn sys_mremap(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    addr: UserAddress,
    old_length: usize,
    new_length: usize,
    flags: u32,
    new_addr: UserAddress,
) -> Result<UserAddress, Errno> {
    let flags = MremapFlags::from_bits(flags).ok_or_else(|| errno!(EINVAL))?;
    let addr =
        current_task.mm().remap(current_task, addr, old_length, new_length, flags, new_addr)?;
    Ok(addr)
}
*/

fit::result<Errno, UserAddress> sys_brk(const CurrentTask& current_task, UserAddress addr) {
  return current_task->mm()->set_brk(current_task, addr);
}

}  // namespace starnix
