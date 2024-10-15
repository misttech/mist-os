// Copyright 2024 Mist Tecnlogia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/memory_file.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/logging/logging.h>
#include <lib/mistos/starnix/kernel/mm/memory.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/anon_node.h>
#include <lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/mount_info.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/math.h>
#include <zircon/errors.h>
#include <zircon/rights.h>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <ktl/span.h>
#include <object/vm_object_dispatcher.h>

#include <ktl/enforce.h>

namespace starnix {

fit::result<Errno, MemoryFileNode*> MemoryFileNode::New() {
  KernelHandle<VmObjectDispatcher> vmo_kernel_handle;
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_rights_t vmo_rights;
  zx_status_t status =
      VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kResizable, 0, &vmo);

  if (status != ZX_OK) {
    return fit::error(MemoryManager::get_errno_for_map_err(status));
  }

  // build and point a dispatcher at it
  status =
      VmObjectDispatcher::Create(ktl::move(vmo), 0, VmObjectDispatcher::InitialMutability::kMutable,
                                 &vmo_kernel_handle, &vmo_rights);

  if (status != ZX_OK) {
    return fit::error(MemoryManager::get_errno_for_map_err(status));
  }

  fbl::AllocChecker ac;
  auto ops = new (&ac)
      MemoryFileNode(MemoryObject::From(Handle::Make(ktl::move(vmo_kernel_handle), vmo_rights)));
  if (!ac.check()) {
    return fit::error(errno(ENOMEM));
  }
  return fit::ok(ops);
}

MemoryFileNode* MemoryFileNode::from_memory(fbl::RefPtr<MemoryObject> memory) {
  fbl::AllocChecker ac;
  auto ops = new (&ac) MemoryFileNode(ktl::move(memory));
  ASSERT(ac.check());
  return ops;
}

void MemoryFileNode::initial_info(FsNodeInfo& info) { info.size = memory_->get_content_size(); }

fit::result<Errno, ktl::unique_ptr<FileOps>> MemoryFileNode::create_file_ops(
    /*FileOpsCore& locked,*/ const FsNode& node, const CurrentTask& current_task,
    OpenFlags _flags) {
  OpenFlagsImpl flags(_flags);
  if (flags.contains(OpenFlagsEnum::TRUNC)) {
    // Truncating to zero length must pass the shrink seal check.
    // node.write_guard_state.lock().check_no_seal(SealFlags::SHRINK) ? ;
  }

  // Produce a VMO handle with rights reduced to those requested in |flags|.
  // TODO(b/319240806): Accumulate required rights, rather than starting from `DEFAULT`.
  auto desired_rights = ZX_DEFAULT_VMO_RIGHTS | ZX_RIGHT_RESIZE;
  if (!flags.can_read()) {
    desired_rights &= ~ZX_RIGHT_READ;
  }
  if (!flags.can_write()) {
    desired_rights &= ~(ZX_RIGHT_WRITE | ZX_RIGHT_RESIZE);
  }

  auto scoped_memory = memory_->duplicate_handle(desired_rights);
  if (scoped_memory.is_error()) {
    return fit::error(errno(EIO));
  }
  auto file_object = MemoryFileObject::New(scoped_memory.value());

  return fit::ok(ktl::unique_ptr<MemoryFileObject>(file_object));
}

fit::result<Errno> MemoryFileNode::truncate(const FsNode& node, const CurrentTask& current_task,
                                            uint64_t length) {
  return fit::error(errno(ENOTSUP));
}

fit::result<Errno> MemoryFileNode::allocate(const FsNode& node, const CurrentTask& current_task,
                                            FallocMode mode, uint64_t offset, uint64_t length) {
  return fit::error(errno(ENOTSUP));
}

MemoryFileObject* MemoryFileObject::New(fbl::RefPtr<MemoryObject> memory) {
  fbl::AllocChecker ac;
  auto ops = new (&ac) MemoryFileObject(ktl::move(memory));
  ASSERT(ac.check());
  return ops;
}

fit::result<Errno, size_t> MemoryFileObject::read(const MemoryObject& memory,
                                                  const FileObject& file, size_t offset,
                                                  OutputBuffer* data) {
  size_t actual;
  auto info = file.node()->info();
  auto file_length = info->size;
  auto want_read = data->available();
  if (offset < file_length) {
    auto to_read = (file_length < offset + want_read) ? (file_length - offset) : (want_read);
    auto buf = memory.read_to_vec(offset, to_read);
    if (buf.is_error()) {
      return fit::error(errno(EIO));
    }
    // drop(info);
    auto result = data->write_all(ktl::span{buf.value().data(), to_read});
    if (result.is_error())
      return result.take_error();
    actual = to_read;
  } else {
    actual = 0;
  }
  return fit::ok(actual);
}

fit::result<Errno, size_t> MemoryFileObject::write(const MemoryObject& memory,
                                                   const FileObject& file,
                                                   const CurrentTask& current_task, size_t offset,
                                                   InputBuffer* data) {
  auto want_write = data->available();
  auto result = data->peek_all();
  if (result.is_error())
    return result.take_error();
  auto buf = ktl::move(result.value());

  auto mutator = [&](FsNodeInfo& info) -> fit::result<Errno, size_t> {
    auto write_end = offset + want_write;
    auto update_content_size = false;

    // We must hold the lock till the end of the operation to guarantee that
    // there is no change to the seals.
    // let state = file.name.entry.node.write_guard_state.lock();

    // Non-zero writes must pass the write seal check.
    if (want_write != 0) {
      // state.check_no_seal(SealFlags::WRITE | SealFlags::FUTURE_WRITE) ? ;
    }

    // Writing past the file size
    if (write_end > info.size) {
      // The grow seal check failed.
      /*
        if let Err(e) = state.check_no_seal(SealFlags::GROW) {
            if offset >= info.size {
                // Write starts outside the file.
                // Forbid because nothing can be written without growing.
                return Err(e);
            } else if info.size == info.storage_size() {
                // Write starts inside file and EOF page does not need to grow.
                // End write at EOF.
                write_end = info.size;
                want_write = write_end - offset;
            } else {
                // Write starts inside file and EOF page needs to grow.
                let eof_page_start = info.storage_size() - (*PAGE_SIZE as usize);

                if offset >= eof_page_start {
                    // Write starts in EOF page.
                    // Forbid because EOF page cannot grow.
                    return Err(e);
                }

                // End write at page before EOF.
                write_end = eof_page_start;
                want_write = write_end - offset;
            }
        }
      */
    }

    // Check against the FSIZE limt
    auto fsize_limit = current_task->thread_group()->get_rlimit({ResourceEnum::FSIZE});
    if (write_end > fsize_limit) {
      if (offset >= fsize_limit) {
        // Write starts beyond the FSIZE limt.
        // send_standard_signal(current_task, SignalInfo::default(SIGXFSZ));
        return fit::error(errno(EFBIG));
      }

      // End write at FSIZE limit.
      write_end = fsize_limit;
      want_write = write_end - offset;
    }

    if (write_end > info.size) {
      if (write_end > info.storage_size()) {
        if (auto result = update_memory_file_size(memory, info, write_end); result.is_error())
          return result.take_error();
      }
      update_content_size = true;
    }

    auto status = memory.write(ktl::span<const uint8_t>{buf.data(), want_write}, offset);
    if (status.is_error()) {
      return fit::error(errno(EIO));
    }

    if (update_content_size) {
      info.size = write_end;
    }
    if (auto result = data->advance(want_write); result.is_error()) {
      return result.take_error();
    }

    return fit::ok(want_write);
  };

  return file.node()->update_info<fit::result<Errno, size_t>>(mutator);
}

fit::result<Errno, fbl::RefPtr<MemoryObject>> MemoryFileObject::get_memory(
    const fbl::RefPtr<MemoryObject>& memory, const FileObject& file, const CurrentTask&,
    ProtectionFlags prot) {
  // In MemoryFileNode::create_file_ops, we downscoped the rights
  // on the VMO to match the rights on the file object. If the caller
  // wants more rights than exist on the file object, return an error
  // instead of returning a MemoryObject that does not conform to
  // the FileOps::get_memory contract.
  if (prot.contains(ProtectionFlagsEnum::READ) && !file.can_read()) {
    return fit::error(errno(EACCES));
  }
  if (prot.contains(ProtectionFlagsEnum::WRITE) && !file.can_write()) {
    return fit::error(errno(EACCES));
  }

  auto mem = memory;
  if (prot.contains(ProtectionFlagsEnum::EXEC)) {
    auto dup = mem->duplicate_handle(ZX_RIGHT_SAME_RIGHTS);
    if (dup.is_error()) {
      return fit::error(impossible_error(dup.error_value()));
    }
    dup = dup->replace_as_executable();
    if (dup.is_error()) {
      return fit::error(impossible_error(dup.error_value()));
    }
    mem = dup.value();
  }

  return fit::ok(mem);
}

fit::result<Errno, FileHandle> new_memfd(const CurrentTask& current_task, FsString name,
                                         SealFlags seals, OpenFlags flags) {
  auto fs = anon_fs(current_task->kernel());

  auto new_result = MemoryFileNode::New();
  if (new_result.is_error())
    return new_result.take_error();

  auto node =
      fs->create_node(current_task, ktl::unique_ptr<FsNodeOps>(new_result.value()),
                      FsNodeInfo::new_factory(FILE_MODE(IFREG, 0600), current_task->as_fscred()));
  /*node.write_guard_state.lock().enable_sealing(seals);*/

  auto open_result = node->open(current_task, MountInfo::detached(), flags, false);
  if (open_result.is_error())
    return open_result.take_error();

  // In /proc/[pid]/fd, the target of this memfd's symbolic link is "/memfd:[name]".
  auto local_name = FsString("/memfd:");
  // local_name += name;

  auto namespace_node = NamespaceNode::new_anonymous(DirEntry::New(node, {}, local_name));

  return FileObject::New(ktl::move(open_result.value()), namespace_node, flags);
}

fit::result<Errno, size_t> update_memory_file_size(const MemoryObject& memory,
                                                   FsNodeInfo& node_info, size_t requested_size) {
  ASSERT(requested_size <= MAX_LFS_FILESIZE);
  auto size = round_up_to_system_page_size(requested_size) _EP(size);
  auto status = memory.set_size(size.value());
  if (status.is_error()) {
    switch (status.error_value()) {
      case ZX_ERR_NO_MEMORY:
      case ZX_ERR_OUT_OF_RANGE:
        return fit::error(errno(ENOMEM));
      default:
        impossible_error(status.error_value());
    }
  }
  node_info.blocks = size.value() / node_info.blksize;
  return fit::ok(size.value());
}

}  // namespace starnix
