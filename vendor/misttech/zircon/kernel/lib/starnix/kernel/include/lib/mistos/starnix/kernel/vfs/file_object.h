// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_OBJECT_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_OBJECT_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/mm/memory.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/vfs/dirent_sink.h>
#include <lib/mistos/starnix/kernel/vfs/namespace_node.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>

#include <fbl/ref_ptr.h>
#include <ktl/functional.h>
#include <ktl/optional.h>
#include <ktl/unique_ptr.h>

class VmObject;

namespace starnix {

size_t const MAX_LFS_FILESIZE = 0x7fffffffffffffff;

fit::result<Errno, size_t> checked_add_offset_and_length(size_t offset, size_t length);

class FileObject;
class OutputBuffer;
class InputBuffer;
class CurrentTask;
class FileOps;
class FileSystem;

using FileSystemHandle = fbl::RefPtr<FileSystem>;
using WeakFileHandle = util::WeakPtr<FileObject>;
using starnix_uapi::OpenFlagsImpl;

enum class SeekTargetType : uint8_t {
  // Seek to the given offset relative to the start of the file.
  Set,
  // Seek to the given offset relative to the current position.
  Cur,
  // Seek to the given offset relative to the end of the file.
  End,
  // Seek for the first data after the given offset,
  Data,
  // Seek for the first hole after the given offset,
  Hole,
};

struct SeekTarget {
  SeekTargetType type;

  off_t offset;
};

struct FileObjectId {
  uint64_t id;
};

// A session with a file object.
//
// Each time a client calls open(), we create a new FileObject from the
// underlying FsNode that receives the open(). This object contains the state
// that is specific to this sessions whereas the underlying FsNode contains
// the state that is shared between all the sessions.
class FileObject : public fbl::RefCounted<FileObject> {
 public:
  /// Weak reference to the `FileHandle` of this `FileObject`. This allows to retrieve the
  /// `FileHandle` from a `FileObject`.
  WeakFileHandle weak_handle_;

  // A unique identifier for this file object.
  FileObjectId id_;

 private:
  ktl::unique_ptr<FileOps> ops_;

 public:
  // The NamespaceNode associated with this FileObject.
  //
  // Represents the name the process used to open this file.
  NamespaceNode name_;

  FileSystemHandle fs_;

  mutable starnix_sync::StarnixMutex<off_t> offset_;

 private:
  mutable starnix_sync::StarnixMutex<OpenFlags> flags_;

  // async_owner: Mutex<FileAsyncOwner>,

  //_file_write_guard: Option<FileWriteGuard>,

 public:
  /// Create a FileObject that is not mounted in a namespace.
  ///
  /// In particular, this will create a new unrooted entries. This should not be used on
  /// file system with persistent entries, as the created entry will be out of sync with the one
  /// from the file system.
  ///
  /// The returned FileObject does not have a name.
  static FileHandle new_anonymous(ktl::unique_ptr<FileOps>, FsNodeHandle node, OpenFlags flags);

  /// Create a FileObject with an associated NamespaceNode.
  ///
  /// This function is not typically called directly. Instead, consider
  /// calling NamespaceNode::open.
  static fit::result<Errno, FileHandle> New(ktl::unique_ptr<FileOps>, NamespaceNode name,
                                            OpenFlags flags);

  /// The FsNode from which this FileObject was created.
  FsNodeHandle node() const;

  bool can_read() const { return OpenFlagsImpl(*flags_.Lock()).can_read(); }

  bool can_write() const { return OpenFlagsImpl(*flags_.Lock()).can_write(); }

  FileOps& ops() const { return *ops_; }

  fit::result<Errno, pid_t> as_pid() const;

  OpenFlags flags() const { return *flags_.Lock(); }

 private:
  /// Common implementation for `read` and `read_at`.
  template <typename ReadFn>
  fit::result<Errno, size_t> read_internal(ReadFn read) const {
    static_assert(std::is_invocable_r_v<fit::result<Errno, size_t>, ReadFn>);

    if (!can_read()) {
      return fit::error(errno(EBADF));
    }

    auto result = read() _EP(result);
    auto bytes_read = result.value();

    // TODO(steveaustin) - omit updating time_access to allow info to be immutable
    // and thus allow simultaneous reads.
    // update_atime();
    if (bytes_read > 0) {
      // notify(InotifyMask::ACCESS);
    }

    return fit::ok(bytes_read);
  }

 public:
  fit::result<Errno, size_t> read(const CurrentTask& current_task, OutputBuffer* data) const;

  fit::result<Errno, size_t> read_at(const CurrentTask& current_task, size_t offset,
                                     OutputBuffer* data) const;

 private:
  /// Common checks before calling ops().write.
  fit::result<Errno, size_t> write_common(const CurrentTask& current_task, size_t offset,
                                          InputBuffer* data) const;

  /// Common wrapper work for `write` and `write_at`.
  template <typename WriteFn>
  fit::result<Errno, size_t> write_fn(const CurrentTask& current_task, WriteFn write) const {
    static_assert(std::is_invocable_r_v<fit::result<Errno, size_t>, WriteFn>);

    if (!can_write()) {
      return fit::error(errno(EBADF));
    }

    // self.node().clear_suid_and_sgid_bits(current_task) ? ;

    auto result = write() _EP(result);
    auto bytes_written = result.value();

    // self.node().update_ctime_mtime();

    if (bytes_written > 0) {
      // self.notify(InotifyMask::MODIFY);
    }

    return fit::ok(bytes_written);
  }

 public:
  fit::result<Errno, size_t> write(const CurrentTask& current_task, InputBuffer* data) const;

  fit::result<Errno, size_t> write_at(const CurrentTask& current_task, size_t offset,
                                      InputBuffer* data) const;

  fit::result<Errno, off_t> seek(const CurrentTask& current_task, SeekTarget target) const;

  fit::result<Errno, fbl::RefPtr<MemoryObject>> get_memory(const CurrentTask& current_task,
                                                           ktl::optional<size_t> length,
                                                           ProtectionFlags prot) const;

  fit::result<Errno, UserAddress> mmap(const CurrentTask& current_task, DesiredAddress addr,
                                       uint64_t vmo_offset, size_t length,
                                       ProtectionFlags prot_flags, MappingOptionsFlags options,
                                       NamespaceNode filename) const;

  ~FileObject();

 private:
  FileObject(WeakFileHandle weak_handle, FileObjectId id, NamespaceNode name, FileSystemHandle fs,
             ktl::unique_ptr<FileOps> ops, OpenFlags flags);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_OBJECT_H_
