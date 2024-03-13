// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/user_copy/internal.h>
#include <trace.h>

#include <arch/riscv64/user_copy.h>
#include <arch/user_copy.h>
#include <kernel/thread.h>
#include <vm/vm.h>

#define LOCAL_TRACE 0

zx_status_t arch_copy_from_user(void* dst, const void* src, size_t len) {
  // The assembly code just does memcpy with fault handling.  This is
  // the security check that an address from the user is actually a
  // valid userspace address so users can't access kernel memory.
  if (!is_user_accessible_range(reinterpret_cast<vaddr_t>(src), len)) {
    return ZX_ERR_INVALID_ARGS;
  }

  return _riscv64_user_copy(dst, src, len, &Thread::Current::Get()->arch().data_fault_resume, 0)
      .status;
}

zx_status_t arch_copy_to_user(void* dst, const void* src, size_t len) {
  if (!is_user_accessible_range(reinterpret_cast<vaddr_t>(dst), len)) {
    return ZX_ERR_INVALID_ARGS;
  }

  return _riscv64_user_copy(dst, src, len, &Thread::Current::Get()->arch().data_fault_resume, 0)
      .status;
}

UserCopyCaptureFaultsResult arch_copy_from_user_capture_faults(void* dst, const void* src,
                                                               size_t len, CopyContext context) {
  // The assembly code just does memcpy with fault handling.  This is
  // the security check that an address from the user is actually a
  // valid userspace address so users can't access kernel memory.
  if (!is_user_accessible_range(reinterpret_cast<vaddr_t>(src), len)) {
    return UserCopyCaptureFaultsResult{ZX_ERR_INVALID_ARGS};
  }

  LTRACEF("dst %p src %p len %zu\n", dst, src, len);

  Riscv64UserCopyRet ret =
      _riscv64_user_copy(dst, src, len, &Thread::Current::Get()->arch().data_fault_resume,
                         RISCV_CAPTURE_USER_COPY_FAULTS_BIT);

  LTRACEF("ret status %d, pf_va %#lx pf_flags %#x\n", ret.status, ret.pf_va, ret.pf_flags);

  // If a fault didn't occur, and ret.status == ZX_OK, this will copy garbage data. It is the
  // responsibility of the caller to check the status and ignore.
  if (ret.status == ZX_OK) {
    return UserCopyCaptureFaultsResult{ZX_OK};
  } else {
    return {ret.status, {ret.pf_va, ret.pf_flags}};
  }
}

UserCopyCaptureFaultsResult arch_copy_to_user_capture_faults(void* dst, const void* src, size_t len,
                                                             CopyContext context) {
  if (!is_user_accessible_range(reinterpret_cast<vaddr_t>(dst), len)) {
    return UserCopyCaptureFaultsResult{ZX_ERR_INVALID_ARGS};
  }

  LTRACEF("dst %p src %p len %zu\n", dst, src, len);

  Riscv64UserCopyRet ret =
      _riscv64_user_copy(dst, src, len, &Thread::Current::Get()->arch().data_fault_resume,
                         RISCV_CAPTURE_USER_COPY_FAULTS_BIT);

  LTRACEF("ret status %d, pf_va %#lx pf_flags %#x\n", ret.status, ret.pf_va, ret.pf_flags);

  // If a fault didn't occur, and ret.status == ZX_OK, this will copy garbage data. It is the
  // responsibility of the caller to check the status and ignore.
  if (ret.status == ZX_OK) {
    return UserCopyCaptureFaultsResult{ZX_OK};
  } else {
    return {ret.status, {ret.pf_va, ret.pf_flags}};
  }
}
