// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <stdint.h>
#include <trace.h>

#include <arch/x86/mmu.h>

#include "../../../priv.h"

#include <asm/prctl.h>
#include <linux/errno.h>

#define LOCAL_TRACE MISTOS_SYSCALLS_GLOBAL_TRACE(0)

long sys_arch_prctl(int option, unsigned long arg2) {
  LTRACEF_LEVEL(2, "option 0x%x arg2 0x%lx\n", option, arg2);

  long ret = 0;

  switch (option) {
    case ARCH_SET_FS: {
      if (unlikely(!x86_is_vaddr_canonical(arg2)))
        return -EPERM;
      write_msr(X86_MSR_IA32_FS_BASE, arg2);
      break;
    }
    case ARCH_SET_GS: {
      if (unlikely(!x86_is_vaddr_canonical(arg2)))
        return -EPERM;
      write_msr(X86_MSR_IA32_KERNEL_GS_BASE, arg2);
      break;
    }
    case ARCH_GET_FS: {
      auto base = read_msr(X86_MSR_IA32_FS_BASE);
      user_out_ptr<unsigned long> out(reinterpret_cast<unsigned long *>(arg2));
      if (out.copy_to_user(base) != ZX_OK)
        return -EFAULT;
      break;
    }
    case ARCH_GET_GS: {
      auto base = read_msr(X86_MSR_IA32_KERNEL_GS_BASE);
      user_out_ptr<unsigned long> out(reinterpret_cast<unsigned long *>(arg2));
      if (out.copy_to_user(base) != ZX_OK)
        return -EFAULT;
      break;
    }
    default: {
      TRACEF("TODO: arch_prctl option 0x%x\n", option);
      return -ENOSYS;
    }
  }
  return ret;
}
