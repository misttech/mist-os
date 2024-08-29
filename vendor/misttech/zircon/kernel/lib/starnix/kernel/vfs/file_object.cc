// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/file_object.h"

#include <lib/mistos/starnix/kernel/task/module.h>
#include <lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h>
#include <lib/mistos/starnix/kernel/vfs/module.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/weak_wrapper.h>

#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <ktl/optional.h>

namespace starnix {

fit::result<Errno, size_t> checked_add_offset_and_length(size_t offset, size_t length) {
  auto end = checked_add(offset, length);
  if (!end.has_value())
    return fit::error(errno(EINVAL));

  if (*end > MAX_LFS_FILESIZE) {
    return fit::error(errno(EINVAL));
  }
  return fit::ok(*end);
}

fit::result<Errno, long> default_fcntl(uint32_t cmd) {
  // track_stub!(TODO("https://fxbug.dev/322875704"), "default fcntl", cmd);
  return fit::error(errno(EINVAL));
}

fit::result<Errno, long> default_ioctl(const FileObject&, const CurrentTask&, uint32_t request,
                                       long arg) {
  return fit::error(errno(ENOTSUP));
}

FileObject::FileObject(zx::vmo v) : offset(0), flags_(OpenFlags::empty()), vmo(std::move(v)) {}

FileObject::FileObject(WeakFileHandle _weak_handle, FileObjectId _id, NamespaceNode _name,
                       FileSystemHandle _fs, ktl::unique_ptr<FileOps> _ops, OpenFlags _flags)
    : weak_handle(std::move(_weak_handle)),
      id(_id),
      ops(ktl::move(_ops)),
      name(std::move(_name)),
      fs(std::move(_fs)),
      offset(0),
      flags_(_flags - OpenFlagsEnum::CREAT) {}

FileHandle FileObject::new_anonymous(ktl::unique_ptr<FileOps> ops, FsNodeHandle node,
                                     OpenFlags flags) {
  ASSERT(!node->fs()->has_permanent_entries());
  auto new_result = New(std::move(ops), NamespaceNode::new_anonymous_unrooted(node), flags);
  ASSERT_MSG(new_result.value(), "Failed to create anonymous FileObject");
  return new_result.value();
}

fit::result<Errno, FileHandle> FileObject::New(ktl::unique_ptr<FileOps> ops, NamespaceNode name,
                                               OpenFlags flags) {
  /*
    let file_write_guard = if flags.can_write() {
        Some(name.entry.node.create_write_guard(FileWriteGuardMode::WriteFile)?)
    } else {
        None
    };
  */
  auto fs = name.entry->node->fs();
  auto kernel = fs->kernel().Lock();
  if (!kernel) {
    return fit::error(errno(ENOENT));
  }
  auto id = FileObjectId{kernel->next_file_object_id.next()};
  fbl::AllocChecker ac;
  auto file =
      fbl::AdoptRef(new (&ac) FileObject(WeakFileHandle(), id, name, fs, ktl::move(ops), flags));
  if (!ac.check()) {
    return fit::error(errno(ENOMEM));
  }
  file->weak_handle = util::WeakPtr<FileObject>(file.get());

  return fit::ok(file);
}

FsNodeHandle FileObject::node() const { return name.entry->node; }

fit::result<Errno, size_t> FileObject::read_internal(
    std::function<fit::result<Errno, size_t>()> read) const {
  if (!can_read()) {
    return fit::error(errno(EBADF));
  }

  auto result = read();
  if (result.is_error())
    return result.take_error();
  auto bytes_read = result.value();

  // TODO(steveaustin) - omit updating time_access to allow info to be immutable
  // and thus allow simultaneous reads.
  // update_atime();
  if (bytes_read > 0) {
    // notify(InotifyMask::ACCESS);
  }

  return fit::ok(bytes_read);
}

fit::result<Errno, size_t> FileObject::read(const CurrentTask& current_task,
                                            OutputBuffer* data) const {
  return read_internal([&]() -> fit::result<Errno, size_t> {
    if (!ops_().has_persistent_offsets()) {
      if (data->available() > MAX_LFS_FILESIZE) {
        return fit::error(errno(EINVAL));
      }
      return ops->read(*this, current_task, 0, data);
    }
    auto offset_guard = offset.Lock();
    auto _offset = static_cast<size_t>(*offset_guard);
    if (auto result = checked_add_offset_and_length(_offset, data->available()); result.is_error())
      return result.take_error();

    auto result = ops->read(*this, current_task, _offset, data);
    if (result.is_error())
      return result.take_error();

    auto read = result.value();
    *offset_guard += static_cast<off_t>(read);
    return fit::ok(read);
  });
}

fit::result<Errno, size_t> FileObject::read_at(const CurrentTask& current_task, size_t _offset,
                                               OutputBuffer* data) const {
  if (!ops_().is_seekable()) {
    return fit::error(errno(ESPIPE));
  }
  if (auto result = checked_add_offset_and_length(_offset, data->available()); result.is_error())
    return result.take_error();

  return read_internal([&]() -> fit::result<Errno, size_t> {
    return ops->read(*this, current_task, _offset, data);
  });
}

fit::result<Errno, size_t> FileObject::write_common(const CurrentTask& current_task, size_t _offset,
                                                    InputBuffer* data) const {
  // We need to cap the size of `data` to prevent us from growing the file too large,
  // according to <https://man7.org/linux/man-pages/man2/write.2.html>:
  //
  //   The number of bytes written may be less than count if, for example, there is
  //   insufficient space on the underlying physical medium, or the RLIMIT_FSIZE resource
  //   limit is encountered (see setrlimit(2)),
  if (auto result = checked_add_offset_and_length(_offset, data->available()); result.is_error())
    return result.take_error();

  return ops_().write(*this, current_task, _offset, data);
}

fit::result<Errno, size_t> FileObject::write_fn(
    const CurrentTask& current_task, std::function<fit::result<Errno, size_t>()> write) const {
  if (!can_write()) {
    return fit::error(errno(EBADF));
  }

  // self.node().clear_suid_and_sgid_bits(current_task) ? ;

  auto result = write();
  if (result.is_error())
    return result.take_error();
  auto bytes_written = result.value();

  // self.node().update_ctime_mtime();

  if (bytes_written > 0) {
    // self.notify(InotifyMask::MODIFY);
  }

  return fit::ok(bytes_written);
}

fit::result<Errno, size_t> FileObject::write(const CurrentTask& current_task,
                                             InputBuffer* data) const {
  return write_fn(current_task, [&]() -> fit::result<Errno, size_t> {
    if (!ops_().has_persistent_offsets()) {
      return write_common(current_task, 0, data);
    }

    auto _offset = offset.Lock();
    size_t bytes_written;
    if (flags().contains(OpenFlagsEnum::APPEND)) {
      // let _guard = self.node().append_lock.write(current_task)?;
      auto seek_result =
          ops_().seek(*this, current_task, *_offset, SeekTarget{SeekTargetType::End, 0});
      if (seek_result.is_error())
        return seek_result.take_error();

      *_offset = seek_result.value();
      auto result = write_common(current_task, static_cast<size_t>(*_offset), data);
      if (result.is_error())
        return result.take_error();
      bytes_written = result.value();
    } else {
      // let _guard = self.node().append_lock.write(current_task)?;
      auto result = write_common(current_task, static_cast<size_t>(*_offset), data);
      if (result.is_error())
        return result.take_error();
      bytes_written = result.value();
    }
    *_offset += static_cast<off_t>(bytes_written);
    return fit::ok(bytes_written);
  });
}

fit::result<Errno, size_t> FileObject::write_at(const CurrentTask& current_task, size_t _offset,
                                                InputBuffer* data) const {
  if (!ops_().is_seekable()) {
    return fit::error(errno(ESPIPE));
  }

  return write_fn(current_task, [&]() -> fit::result<Errno, size_t> {
    // let _guard = self.node().append_lock.read(current_task) ? ;

    // According to LTP test pwrite04:
    //
    //   POSIX requires that opening a file with the O_APPEND flag should have no effect on the
    //   location at which pwrite() writes data. However, on Linux, if a file is opened with
    //   O_APPEND, pwrite() appends data to the end of the file, regardless of the value of offset.
    if (flags().contains(OpenFlagsEnum::APPEND) && ops_().is_seekable()) {
      if (auto result = checked_add_offset_and_length(_offset, data->available());
          result.is_error())
        return result.take_error();

      auto eof_result = default_eof_offset(*this, current_task);
      if (eof_result.is_error())
        return eof_result.take_error();
      _offset = static_cast<size_t>(eof_result.value());
    }

    return write_common(current_task, _offset, data);
  });
}

fit::result<Errno, off_t> FileObject::seek(const CurrentTask& current_task,
                                           SeekTarget target) const {
  if (!ops_().is_seekable()) {
    return fit::error(errno(ESPIPE));
  }

  if (!ops_().has_persistent_offsets()) {
    return ops_().seek(*this, current_task, 0, target);
  }

  auto offset_guard = offset.Lock();
  auto seek_result = ops_().seek(*this, current_task, *offset_guard, target);
  if (seek_result.is_error())
    return seek_result.take_error();

  auto new_offset = seek_result.value();
  *offset_guard = new_offset;
  return fit::ok(new_offset);
}

fit::result<Errno, off_t> default_eof_offset(const FileObject& file,
                                             const CurrentTask& current_task) {
  auto stat_result = file.node()->stat(current_task);
  if (stat_result.is_error())
    return stat_result.take_error();
  return fit::ok(static_cast<off_t>(stat_result->st_size));
}

fit::result<Errno, off_t> default_seek(
    off_t current_offset, SeekTarget target,
    std::function<fit::result<Errno, off_t>(off_t)> compute_end) {
  auto lresult = [&]() -> fit::result<Errno, ktl::optional<uint64_t>> {
    switch (target.type) {
      case SeekTargetType::Set:
        return fit::ok(target.offset);
      case SeekTargetType::Cur:
        return fit::ok(checked_add(current_offset, target.offset));
      case SeekTargetType::End: {
        auto result = compute_end(target.offset);
        if (result.is_error())
          return result.take_error();
        return fit::ok(result.value());
      }
      case SeekTargetType::Data: {
        auto eof = compute_end(0).value_or(std::numeric_limits<off_t>::max());
        if (target.offset >= eof) {
          return fit::error(errno(ENXIO));
        }
        return fit::ok(target.offset);
      }
      case SeekTargetType::Hole:
        auto eof_result = compute_end(0);
        if (eof_result.is_error())
          return eof_result.take_error();
        auto eof = eof_result.value();
        if (target.offset >= eof) {
          return fit::error(errno(ENXIO));
        }
        return fit::ok(eof);
    }
  }();

  if (lresult.is_error())
    return lresult.take_error();

  auto new_offset = lresult.value();
  if (!new_offset.has_value()) {
    return fit::error(errno(EINVAL));
  }

  if (new_offset < 0) {
    return fit::error(errno(EINVAL));
  }

  return fit::ok(*new_offset);
}

OPathOps* OPathOps::New() {
  fbl::AllocChecker ac;
  auto ops = new (&ac) OPathOps();
  ASSERT(ac.check());
  return ops;
}

}  // namespace starnix
