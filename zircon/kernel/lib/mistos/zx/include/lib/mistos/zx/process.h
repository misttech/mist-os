// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_PROCESS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_PROCESS_H_

#include <lib/mistos/util/process.h>
#include <lib/mistos/zx/object.h>
#include <lib/mistos/zx/task.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>

#include <object/process_dispatcher.h>

namespace zx {
class job;
class thread;

class process final : public task<process> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_PROCESS;

  constexpr process() = default;

  explicit process(fbl::RefPtr<ProcessDispatcher> value) : task(value) {}

  process(process&& other) : task(other.release()) {}

  process& operator=(process&& other) {
    reset(other.release());
    return *this;
  }

  static zx_status_t create(const job& job, const char* name, uint32_t name_len, uint32_t flags,
                            process* proc, vmar* root_vmar);

  zx_status_t start(const thread& thread_handle, uintptr_t entry, uintptr_t stack,
                    /*handle arg_handle*/ uintptr_t arg1, uintptr_t arg2) const;

  zx_status_t read_memory(uintptr_t vaddr, void* buffer, size_t len, size_t* actual) const {
    // return zx_process_read_memory(get(), vaddr, buffer, len, actual);
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t write_memory(uintptr_t vaddr, const void* buffer, size_t len, size_t* actual) const {
    // return zx_process_write_memory(get(), vaddr, buffer, len, actual);
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Provide strongly-typed overload, in addition to get_child(handle*).
  using task<process>::get_child;
  zx_status_t get_child(uint64_t koid, zx_rights_t rights, thread* result) const;

  static inline unowned<process> self() { return unowned<process>(zx_process_self()); }

  zx_status_t get_info(uint32_t topic, void* buffer, size_t buffer_size, size_t* actual_count,
                       size_t* avail_count) const final;
};

using unowned_process = unowned<process>;

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_PROCESS_H_
