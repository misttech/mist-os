// Copyright 2024 Mist Tecnlogia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/vmo_file.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/module.h>
#include <lib/mistos/starnix/kernel/vfs/anon_node.h>
#include <lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/module.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/math.h>
#include <zircon/errors.h>

#include <vector>

#include <ktl/span.h>

namespace starnix {

fit::result<Errno, VmoFileNode*> VmoFileNode::New() {
  zx::vmo vmo;
  zx_status_t status = zx::vmo::create(0, ZX_VMO_RESIZABLE, &vmo);
  if (status != ZX_OK) {
    return fit::error(errno(ENOMEM));
  }

  fbl::AllocChecker ac;
  auto ops = new (&ac) VmoFileNode(std::move(vmo) /*, MemoryXattrStorage::Default()*/);
  if (!ac.check()) {
    return fit::error(errno(ENOMEM));
  }
  return fit::ok(ops);
}

VmoFileNode* VmoFileNode::from_vmo(zx::vmo vmo) {
  fbl::AllocChecker ac;
  auto ops = new (&ac) VmoFileNode(std::move(vmo) /*, MemoryXattrStorage::Default()*/);
  ASSERT(ac.check());
  return ops;
}

void VmoFileNode::initial_info(FsNodeInfo& info) {}

fit::result<Errno, ktl::unique_ptr<FileOps>> VmoFileNode::create_file_ops(
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
    /*desired_rights.remove(zx::Rights::READ);*/
  }
  if (!flags.can_write()) {
    /*desired_rights.remove(zx::Rights::WRITE | zx::Rights::RESIZE);*/
  }

  zx::vmo scoped_vmo;
  zx_status_t status = vmo_.duplicate(desired_rights, &scoped_vmo);
  if (status != ZX_OK) {
    return fit::error(errno(EIO));
  }

  auto file_object = VmoFileObject::New(std::move(scoped_vmo));
  return fit::ok(ktl::unique_ptr<VmoFileObject>(file_object));
}

fit::result<Errno> VmoFileNode::truncate(const FsNode& node, const CurrentTask& current_task,
                                         uint64_t length) {
  return fit::error(errno(ENOTSUP));
}

fit::result<Errno> VmoFileNode::allocate(const FsNode& node, const CurrentTask& current_task,
                                         FallocMode mode, uint64_t offset, uint64_t length) {
  return fit::error(errno(ENOTSUP));
}

VmoFileObject* VmoFileObject::New(zx::vmo vmo) {
  fbl::AllocChecker ac;
  auto ops = new (&ac) VmoFileObject(std::move(vmo));
  ASSERT(ac.check());
  return ops;
}

fit::result<Errno, size_t> VmoFileObject::read(const zx::vmo& vmo, const FileObject& file,
                                               size_t offset, OutputBuffer* data) {
  size_t actual;
  auto info = file.node()->info();
  auto file_length = info->size;
  auto want_read = data->available();
  if (offset < file_length) {
    auto to_read = (file_length < offset + want_read) ? (file_length - offset) : (want_read);
    std::vector<uint8_t> buf(to_read);
    zx_status_t status = vmo.read(buf.data(), offset, to_read);
    if (status != ZX_OK) {
      return fit::error(errno(EIO));
    }
    // drop(info);
    auto result = data->write_all(ktl::span{buf.data(), buf.size()});
    if (result.is_error())
      return result.take_error();
    actual = to_read;
  } else {
    actual = 0;
  }
  return fit::ok(actual);
}

fit::result<Errno, size_t> VmoFileObject::write(const zx::vmo& vmo, const FileObject& file,
                                                const CurrentTask& current_task, size_t offset,
                                                InputBuffer* data) {
  auto want_write = data->available();
  auto result = data->peek_all();
  if (result.is_error())
    return result.take_error();
  auto buf = ktl::move(result.value());

  return file.node()->update_info<fit::result<Errno, size_t>>(
      [&](FsNodeInfo& info) -> fit::result<Errno, size_t> {
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
        auto fsize_limit = current_task->thread_group->get_rlimit({ResourceEnum::FSIZE});
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
            if (auto result = update_vmo_file_size(vmo, info, write_end); result.is_error())
              return result.take_error();
          }
          update_content_size = true;
        }
        auto status = vmo.write(buf.data(), offset, want_write);
        if (status != ZX_OK) {
          return fit::error(errno(EIO));
        }

        if (update_content_size) {
          info.size = write_end;
        }
        if (auto result = data->advance(want_write); result.is_error())
          return result.take_error();

        return fit::ok(want_write);
      });
}

fit::result<Errno, zx::vmo> VmoFileObject::get_vmo(const zx::vmo& vmo, const FileObject&,
                                                   const CurrentTask&, ProtectionFlags prot) {
  return fit::error(errno(ENOTSUP));
}

fit::result<Errno, FileHandle> new_memfd(const CurrentTask& current_task, FsString name,
                                         SealFlags seals, OpenFlags flags) {
  auto fs = anon_fs(current_task->kernel());

  auto new_result = VmoFileNode::New();
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
  local_name += name;

  auto namespace_node = NamespaceNode::new_anonymous(DirEntry::New(node, {}, local_name));

  return FileObject::New(std::move(open_result.value()), namespace_node, flags);
}

fit::result<Errno, size_t> update_vmo_file_size(const zx::vmo& vmo, FsNodeInfo& node_info,
                                                size_t requested_size) {
  ASSERT(requested_size <= MAX_LFS_FILESIZE);
  auto size_result = round_up_to_system_page_size(requested_size);
  if (size_result.is_error())
    return size_result.take_error();
  auto size = size_result.value();
  zx_status_t status = vmo.set_size(size);
  if (status != ZX_OK) {
    switch (status) {
      case ZX_ERR_NO_MEMORY:
      case ZX_ERR_OUT_OF_RANGE:
        return fit::error(errno(ENOMEM));
      default:
        PANIC("encountered impossible error: %d", status);
    }
  }
  node_info.blocks = size / node_info.blksize;
  return fit::ok(size);
}

}  // namespace starnix
