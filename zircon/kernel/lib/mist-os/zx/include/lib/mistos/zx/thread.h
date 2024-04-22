// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_THREAD_H_
#define ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_THREAD_H_

#include <lib/mistos/zx/object.h>
#include <lib/mistos/zx/task.h>

#include <object/thread_dispatcher.h>

namespace zx {
class process;

class thread final : public task<thread> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_THREAD;

  constexpr thread() = default;

  explicit thread(fbl::RefPtr<ThreadDispatcher> value) : task(value) {}

  thread(thread&& other) : task(other.release()) {}

  thread& operator=(thread&& other) {
    reset(other.release());
    return *this;
  }

  // Rather than creating a thread directly with this syscall, consider using
  // std::thread or thrd_create, which properly integrates with the
  // thread-local data structures in libc.
  static zx_status_t create(const process& process, const char* name, uint32_t name_len,
                            uint32_t flags, thread* result);

  // The first variant maps exactly to the syscall and can be used for
  // launching threads in remote processes. The second variant is for
  // conveniently launching threads in the current process.
  zx_status_t start(uintptr_t thread_entry, uintptr_t stack, uintptr_t arg1, uintptr_t arg2) const {
    /*LTRACEF("handle %p, entry %#" PRIxPTR ", sp %#" PRIxPTR ", arg1 %#" PRIxPTR ", arg2 %#"
       PRIxPTR
            "\n",
            get().get(), thread_entry, stack, arg1, arg2);*/
    return get()->Start(ThreadDispatcher::EntryState{thread_entry, stack, arg1, arg2},
                        /* ensure_initial_thread= */ false);
  }
  zx_status_t start(void (*thread_entry)(uintptr_t arg1, uintptr_t arg2), void* stack,
                    uintptr_t arg1, uintptr_t arg2) const {
    return start(reinterpret_cast<uintptr_t>(thread_entry), reinterpret_cast<uintptr_t>(stack),
                 arg1, arg2);
  }

  zx_status_t read_state(uint32_t kind, void* buffer, size_t len) const {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t write_state(uint32_t kind, const void* buffer, size_t len) const {
    return ZX_ERR_NOT_SUPPORTED;
  }

  static inline unowned<thread> self() { return unowned<thread>(nullptr); }
};

using unowned_thread = unowned<thread>;

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_THREAD_H_
