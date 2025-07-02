// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "backtrace.h"

#include <lib/arch/backtrace.h>

#include "threads_impl.h"

namespace __libc_sanitizer {

size_t BacktraceByFramePointer(std::span<uintptr_t> pcs) {
  struct IsOnStack {
    bool operator()(const arch::CallFrame* fp) const {
      const iovec& stack = __pthread_self()->safe_stack;
      if (stack.iov_len < sizeof(*fp)) [[unlikely]] {
        // This should be impossible, but assume nothing in a critical
        // error-reporting path since this might be used after clobberation.
        return false;
      }
      const uintptr_t base = reinterpret_cast<uintptr_t>(stack.iov_base);
      const uintptr_t frame = reinterpret_cast<uintptr_t>(fp);
      return frame >= base && frame - base <= stack.iov_len - sizeof(*fp);
    }
  };
  using FpBacktrace = arch::FramePointerBacktrace<IsOnStack>;

  return arch::StoreBacktrace(FpBacktrace::BackTrace(), pcs, __builtin_return_address(0));
}

#if __has_feature(shadow_call_stack)

size_t BacktraceByShadowCallStack(std::span<uintptr_t> pcs) {
  const iovec& shadow_call_stack_block = __pthread_self()->shadow_call_stack;
  return arch::StoreBacktrace(
      arch::ShadowCallStackBacktrace{
          {static_cast<const uintptr_t*>(shadow_call_stack_block.iov_base),
           shadow_call_stack_block.iov_len / sizeof(uintptr_t)},
          arch::GetShadowCallStackPointer()},
      pcs, __builtin_return_address(0));
}

#endif  // __has_feature(shadow_call_stack)

}  // namespace __libc_sanitizer
