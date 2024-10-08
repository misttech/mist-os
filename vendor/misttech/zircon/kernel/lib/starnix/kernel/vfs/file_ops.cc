// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/file_ops.h"

#include <lib/mistos/starnix/kernel/logging/logging.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/util/num.h>

namespace starnix {

fit::result<Errno, UserAddress> FileOps::mmap(const FileObject& file,
                                              const CurrentTask& current_task, DesiredAddress addr,
                                              uint64_t memory_offset, size_t length,
                                              ProtectionFlags prot_flags,
                                              MappingOptionsFlags options,
                                              NamespaceNode filename) const {
  // profile_duration !("FileOpsDefaultMmap");
  // trace_duration !(CATEGORY_STARNIX_MM, c "FileOpsDefaultMmap");
  auto roud_up_length = round_up_to_system_page_size(length) _EP(roud_up_length);
  auto min_memory_size = mtl::checked_add(static_cast<size_t>(memory_offset), *roud_up_length);
  if (!min_memory_size.has_value()) {
    return fit::error(errno(EINVAL));
  }

  auto memory = [&]() -> fit::result<Errno, fbl::RefPtr<MemoryObject>> {
    if (options.contains(MappingOptions::SHARED)) {
      // profile_duration!("GetSharedVmo");
      // trace_duration!(CATEGORY_STARNIX_MM, c"GetSharedVmo");
      auto result = this->get_memory(file, current_task, {min_memory_size}, prot_flags) _EP(result);
      return fit::ok(result.value());
    }
    // profile_duration!("GetPrivateVmo");
    // trace_duration!(CATEGORY_STARNIX_MM, c"GetPrivateVmo");
    //  TODO(tbodt): Use PRIVATE_CLONE to have the filesystem server do the clone for us.
    auto base_prot_flags = (prot_flags | ProtectionFlagsEnum::READ) - ProtectionFlagsEnum::WRITE;
    auto memory = get_memory(file, current_task, {min_memory_size}, base_prot_flags) _EP(memory);
    auto clone_flags = ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE;
    if (!prot_flags.contains(ProtectionFlagsEnum::WRITE)) {
      clone_flags |= ZX_VMO_CHILD_NO_WRITE;
    }
    // trace_duration!(CATEGORY_STARNIX_MM, c"CreatePrivateChildVmo");
    auto result = memory->create_child(clone_flags, 0, memory->get_size());
    if (result.is_error()) {
      impossible_error(result.error_value());
    }
    return fit::ok(result.value());
  }() _EP(memory);

  // Write guard is necessary only for shared mappings. Note that this doesn't depend on
  // `prot_flags` since these can be changed later with `mprotect()`.
  /*
    let file_write_guard = if options.contains(MappingOptions::SHARED) && file.can_write() {
        profile_duration!("AcquireFileWriteGuard");
        let node = &file.name.entry.node;
        let mut state = node.write_guard_state.lock();

        // `F_SEAL_FUTURE_WRITE` should allow `mmap(PROT_READ)`, but block
        // `mprotect(PROT_WRITE)`. This is different from `F_SEAL_WRITE`, which blocks
        // `mmap(PROT_READ)`. To handle this case correctly remove `WRITE` right from the
        // VMO handle to ensure `mprotect(PROT_WRITE)` fails.
        let seals = state.get_seals().unwrap_or(SealFlags::empty());
        if seals.contains(SealFlags::FUTURE_WRITE)
            && !seals.contains(SealFlags::WRITE)
            && !prot_flags.contains(ProtectionFlags::WRITE)
        {
            let mut new_rights = zx::Rights::VMO_DEFAULT - zx::Rights::WRITE;
            if prot_flags.contains(ProtectionFlags::EXEC) {
                new_rights |= zx::Rights::EXECUTE;
            }
            memory = Arc::new(memory.duplicate_handle(new_rights).map_err(impossible_error)?);

            FileWriteGuardRef(None)
        } else {
            state.create_write_guard(node.clone(), FileWriteGuardMode::WriteMapping)?.into_ref()
        }
    } else {
        FileWriteGuardRef(None)
    };
  */

  return current_task->mm()->map_memory(addr, memory.value(), memory_offset, length, prot_flags,
                                        options, {.type = MappingNameType::File});
}

}  // namespace starnix
