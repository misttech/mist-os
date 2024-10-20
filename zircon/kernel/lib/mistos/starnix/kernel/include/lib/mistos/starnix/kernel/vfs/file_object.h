// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_OBJECT_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_OBJECT_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/sync/locks.h>
#include <lib/mistos/starnix/kernel/vfs/direct_sink.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/zx/vmo.h>

#include <functional>

#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>

namespace starnix {

size_t const MAX_LFS_FILESIZE = 0x7fffffffffffffff;

fit::result<Errno, size_t> checked_add_offset_and_length(size_t offset, size_t length);

enum class SeekTargetType {
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

class FileObject;
fit::result<Errno, long> default_fcntl(uint32_t cmd);
fit::result<Errno, long> default_ioctl(const FileObject&, const CurrentTask&, uint32_t request,
                                       long arg);

class OutputBuffer;
class InputBuffer;

/// Corresponds to struct file_operations in Linux, plus any filesystem-specific data.
class FileOps {
 public:
  /// Called when the FileObject is closed.
  virtual void close(const FileObject& file, const CurrentTask& current_task) {}

  /// Called every time close() is called on this file, even if the file is not ready to be
  /// released.
  virtual void flush(const FileObject& file, const CurrentTask& current_task) {}

  /// Returns whether the file has meaningful seek offsets. Returning `false` is only
  /// optimization and will makes `FileObject` never hold the offset lock when calling `read` and
  /// `write`.
  virtual bool has_persistent_offsets() const { return is_seekable(); }

  /// Returns whether the file is seekable.
  virtual bool is_seekable() const = 0;

  /// Read from the file at an offset. If the file does not have persistent offsets (either
  /// directly, or because it is not seekable), offset will be 0 and can be ignored.
  /// Returns the number of bytes read.
  virtual fit::result<Errno, size_t> read(/*Locked<FileOpsCore>& locked,*/ const FileObject& file,
                                          const CurrentTask& current_task, size_t offset,
                                          OutputBuffer* data) = 0;

  /// Write to the file with an offset. If the file does not have persistent offsets (either
  /// directly, or because it is not seekable), offset will be 0 and can be ignored.
  /// Returns the number of bytes written.
  virtual fit::result<Errno, size_t> write(/*Locked<WriteOps>& locked,*/ const FileObject& file,
                                           const CurrentTask& current_task, size_t offset,
                                           InputBuffer* data) = 0;

  /// Adjust the `current_offset` if the file is seekable.
  virtual fit::result<Errno, off_t> seek(const FileObject& file, const CurrentTask& current_task,
                                         off_t current_offset, SeekTarget target) = 0;

  /// Syncs cached state associated with the file descriptor to persistent storage.
  ///
  /// The method blocks until the synchronization is complete.
  virtual fit::result<Errno> sync(const FileObject& file, const CurrentTask& current_task) {
    /*if (!file.node().is_reg() && !file.node().is_dir()) {
      return fit::error(errno(EINVAL));
    }*/
    return fit::ok();
  }

  /// Syncs cached data, and only enough metadata to retrieve said data, to persistent storage.
  ///
  /// The method blocks until the synchronization is complete.
  virtual fit::result<Errno> data_sync(const FileObject& file, const CurrentTask& current_task) {
    return sync(file, current_task);
  }

  /// Returns a VMO representing this file. At least the requested protection flags must
  /// be set on the VMO. Reading or writing the VMO must read or write the file. If this is not
  /// possible given the requested protection, an error must be returned.
  /// The `length` is a hint for the desired size of the VMO. The returned VMO may be larger or
  /// smaller than the requested length.
  /// This method is typically called by [`Self::mmap`].
  virtual fit::result<Errno, zx::vmo> get_vmo(const FileObject& file,
                                              const CurrentTask& current_task,
                                              std::optional<size_t> length, ProtectionFlags prot) {
    return fit::error(errno(ENODEV));
  }

  // Responds to an mmap call. The default implementation calls [`Self::get_vmo`] to get a VMO
  /// and then maps it with [`crate::mm::MemoryManager::map`].
  /// Only implement this trait method if your file needs to control mapping, or record where
  /// a VMO gets mapped.
  virtual fit::result<Errno, UserAddress> mmap(const FileObject& file,
                                               const CurrentTask& current_task, DesiredAddress addr,
                                               uint64_t vmo_offset, size_t length,
                                               ProtectionFlags prot_flags, MappingOptions options,
                                               NamespaceNode filename) {
    return fit::error(errno(ENOTSUP));
  }

  /// Respond to a `getdents` or `getdents64` calls.
  ///
  /// The `file.offset` lock will be held while entering this method. The implementation must look
  /// at `sink.offset()` to read the current offset into the file.
  virtual fit::result<Errno> readdir(const FileObject& file, const CurrentTask& current_task,
                                     DirentSink& sink) {
    return fit::error(errno(ENOTDIR));
  }

#if 0
  /// Establish a one-shot, edge-triggered, asynchronous wait for the given FdEvents for the
  /// given file and task. Returns `None` if this file does not support blocking waits.
  ///
  /// Active events are not considered. This is similar to the semantics of the
  /// ZX_WAIT_ASYNC_EDGE flag on zx_wait_async. To avoid missing events, the caller must call
  /// query_events after calling this.
  ///
  /// If your file does not support blocking waits, leave this as the default implementation.
  virtual ktl::optional<WaitCanceler> wait_async(const FileObject& file,
                                                 const CurrentTask& current_task,
                                                 const Waiter& waiter, FdEvents events,
                                                 EventHandler handler) {
    return std::nullopt;
  }

  /// The events currently active on this file.
  ///
  /// If this function returns `POLLIN` or `POLLOUT`, then FileObject will
  /// add `POLLRDNORM` and `POLLWRNORM`, respective, which are equivalent in
  /// the Linux UAPI.
  ///
  /// See https://linux.die.net/man/2/poll
  virtual fit::result<Errno, FdEvents> query_events(const FileObject& file,
                                                    const CurrentTask& current_task) {
    return {FdEvents::POLLIN | FdEvents::POLLOUT, Errno{}};
  }
#endif

  virtual fit::result<Errno, long> ioctl(/*Locked<FileOpsCore>& locked,*/ const FileObject& file,
                                         const CurrentTask& current_task, uint32_t request,
                                         long arg) {
    return default_ioctl(file, current_task, request, arg);
  }

  virtual fit::result<Errno, long> fcntl(const FileObject& file, const CurrentTask& current_task,
                                         uint32_t cmd, uint64_t arg) {
    return default_fcntl(cmd);
  }

#if 0
  virtual std::pair<std::optional<zx::Handle>, Errno> to_handle(const FileObject& file,
                                                                const CurrentTask& current_task) {
    auto locked = Unlocked<FileOpsCore>{};  // TODO: FileOpsToHandle before FileOpsCore
    auto [handle, err] = serve_file(locked, current_task, file);
    if (err != Errno{}) {
      return {std::nullopt, err};
    }
    return {std::make_optional(std::move(handle)), Errno{}};
  }
#endif

  /// Returns the associated pid_t.
  ///
  /// Used by pidfd and `/proc/<pid>`. Unlikely to be used by other files.
  virtual fit::result<Errno, pid_t> as_pid(const FileObject& file) {
    return fit::error(errno(EBADF));
  }

  virtual fit::result<Errno> readahead(const FileObject& file, const CurrentTask& current_task,
                                       size_t offset, size_t length) {
    return fit::error(errno(EINVAL));
  }

 public:
  // C++
  virtual ~FileOps() = default;
};

fit::result<Errno, off_t> default_eof_offset(const FileObject& file,
                                             const CurrentTask& current_task);

/// Implement the seek method for a file. The computation from the end of the file must be provided
/// through a callback.
///
/// Errors if the calculated offset is invalid.
///
/// - `current_offset`: The current position
/// - `target`: The location to seek to.
/// - `compute_end`: Compute the new offset from the end. Return an error if the operation is not
///    supported.
fit::result<Errno, off_t> default_seek(off_t current_offset, SeekTarget targe,
                                       std::function<fit::result<Errno, off_t>(off_t)> compute_end);

#define fileops_impl_delegate_read_and_seek(delegate)                                      \
  bool is_seekable() const final { return true; }                                          \
                                                                                           \
  fit::result<Errno, size_t> read(/*Locked<FileOpsCore>& locked,*/ const FileObject& file, \
                                  const CurrentTask& current_task, size_t offset,          \
                                  OutputBuffer* data) final {                              \
    return delegate.read(locked, file, current_task, offset, data);                        \
  }                                                                                        \
                                                                                           \
  fit::result<Errno, off_t> seek(const FileObject& file, const CurrentTask& current_task,  \
                                 off_t current_offset, SeekTarget target) final {          \
    return delegate.seek(file, current_task, current_offset, target);                      \
  }                                                                                        \
  using __fileops_impl_delegate_read_and_seek_force_semicolon = int

/// Implements [`FileOps::seek`] in a way that makes sense for seekable files.
#define fileops_impl_seekable()                                                                  \
  bool is_seekable() const final { return true; }                                                \
                                                                                                 \
  fit::result<Errno, off_t> seek(const FileObject& file, const CurrentTask& current_task,        \
                                 off_t current_offset, SeekTarget target) final {                \
    return default_seek(current_offset, target, [&](off_t offset) -> fit::result<Errno, off_t> { \
      auto result = default_eof_offset(file, current_task);                                      \
      if (result.is_error())                                                                     \
        return result.take_error();                                                              \
                                                                                                 \
      auto eof_offset = result.value();                                                          \
      auto offset_opt = checked_add(offset, eof_offset);                                         \
      if (!offset_opt.has_value())                                                               \
        return fit::error(errno(EINVAL));                                                        \
      return fit::ok(offset_opt.value());                                                        \
    });                                                                                          \
  }                                                                                              \
  using __fileops_impl_seekable_force_semicolon = int

/// Implements [`FileOps`] methods in a way that makes sense for non-seekable files.
#define fileops_impl_nonseekable()                                                        \
  bool is_seekable() const final { return false; }                                        \
                                                                                          \
  fit::result<Errno, off_t> seek(const FileObject& file, const CurrentTask& current_task, \
                                 off_t current_offset, SeekTarget target) final {         \
    return fit::error(errno(ESPIPE));                                                     \
  }                                                                                       \
  using __fileops_impl_nonseekable_force_semicolon = int

/// Implements [`FileOps::seek`] methods in a way that makes sense for files that ignore
/// seeking operations and always read/write at offset 0.
#define fileops_impl_seekless()                                                           \
  bool has_persistent_offsets() const final { return false; }                             \
                                                                                          \
  bool is_seekable() const final { return true; }                                         \
                                                                                          \
  fit::result<Errno, off_t> seek(const FileObject& file, const CurrentTask& current_task, \
                                 off_t current_offset, SeekTarget target) final {         \
    return fit::ok(0);                                                                    \
  }                                                                                       \
  using __fileops_impl_seekless_force_semicolon = int

#define fileops_impl_dataless()                                                            \
  fit::result<Errno, size_t> read(/*Locked<FileOpsCore>& locked,*/ const FileObject& file, \
                                  const CurrentTask& current_task, size_t offset,          \
                                  OutputBuffer& data) final {                              \
    return fit::error(errno(EINVAL));                                                      \
  }                                                                                        \
                                                                                           \
  fit::result<Errno, size_t> write(/*Locked<WriteOps>& locked,*/ const FileObject& file,   \
                                   const CurrentTask& current_task, size_t offset,         \
                                   InputBuffer& data) final {                              \
    return fit::error(errno(EINVAL));                                                      \
  }                                                                                        \
  using __fileops_impl_dataless_force_semicolon = int

#define fileops_impl_directory()                                                           \
  bool is_seekable() const final { return true; }                                          \
                                                                                           \
  fit::result<Errno, size_t> read(/*Locked<FileOpsCore>& locked,*/ const FileObject& file, \
                                  const CurrentTask& current_task, size_t offset,          \
                                  OutputBuffer& data) final {                              \
    return fit::error(errno(EISDIR));                                                      \
  }                                                                                        \
                                                                                           \
  fit::result<Errno, size_t> write(/*Locked<WriteOps>& locked,*/ const FileObject& file,   \
                                   const CurrentTask& current_task, size_t offset,         \
                                   InputBuffer* data) final {                              \
    return fit::error(errno(EISDIR));                                                      \
  }                                                                                        \
  using __fileops_impl_directory_force_semicolon = int

struct OPathOps : FileOps {
  /// impl OPathOps
  static OPathOps* New();

  /// impl FileOps
  bool has_persistent_offsets() const final { return false; }
  bool is_seekable() const final { return true; }
  fit::result<Errno, size_t> read(/*Locked<FileOpsCore>& locked,*/ const FileObject& file,
                                  const CurrentTask& current_task, size_t offset,
                                  OutputBuffer* data) final {
    return fit::error(errno(EBADF));
  }
  fit::result<Errno, size_t> write(/*Locked<WriteOps>& locked,*/ const FileObject& file,
                                   const CurrentTask& current_task, size_t offset,
                                   InputBuffer* data) final {
    return fit::error(errno(EBADF));
  }
  fit::result<Errno, off_t> seek(const FileObject& file, const CurrentTask& current_task,
                                 off_t current_offset, SeekTarget target) final {
    return fit::error(errno(EBADF));
  }
  fit::result<Errno, zx::vmo> get_vmo(const FileObject& file, const CurrentTask& current_task,
                                      std::optional<size_t> length, ProtectionFlags prot) final {
    return fit::error(errno(EBADF));
  }
  fit::result<Errno> readdir(const FileObject& file, const CurrentTask& current_task,
                             DirentSink& sink) final {
    return fit::error(errno(EBADF));
  }
  fit::result<Errno, long> ioctl(/*Locked<FileOpsCore>& locked,*/ const FileObject& file,
                                 const CurrentTask& current_task, uint32_t request,
                                 long arg) final {
    return fit::error(errno(EBADF));
  }
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
  WeakFileHandle weak_handle;

  // A unique identifier for this file object.
  FileObjectId id;

 private:
  ktl::unique_ptr<FileOps> ops;

 public:
  // The NamespaceNode associated with this FileObject.
  //
  // Represents the name the process used to open this file.
  NamespaceNode name;

  FileSystemHandle fs;

  mutable StarnixMutex<off_t> offset;

 private:
  mutable StarnixMutex<OpenFlags> flags_;

  // async_owner: Mutex<FileAsyncOwner>,

  //_file_write_guard: Option<FileWriteGuard>,

 public:
  // (FIXME: Herrera) This ctor to handle VMO must be deleted soon.
  FileObject(zx::vmo v);
  zx::vmo vmo;

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

  FileOps& ops_() const { return *ops.get(); }

  OpenFlags flags() const { return *flags_.Lock(); }

 private:
  /// Common implementation for `read` and `read_at`.
  fit::result<Errno, size_t> read_internal(std::function<fit::result<Errno, size_t>()> read) const;

 public:
  fit::result<Errno, size_t> read(const CurrentTask& current_task, OutputBuffer* data) const;

  fit::result<Errno, size_t> read_at(const CurrentTask& current_task, size_t offset,
                                     OutputBuffer* data) const;

 private:
  /// Common checks before calling ops().write.
  fit::result<Errno, size_t> write_common(const CurrentTask& current_task, size_t offset,
                                          InputBuffer* data) const;

  /// Common wrapper work for `write` and `write_at`.
  fit::result<Errno, size_t> write_fn(const CurrentTask& current_task,
                                      std::function<fit::result<Errno, size_t>()> write) const;

 public:
  fit::result<Errno, size_t> write(const CurrentTask& current_task, InputBuffer* data) const;

  fit::result<Errno, size_t> write_at(const CurrentTask& current_task, size_t offset,
                                      InputBuffer* data) const;

  fit::result<Errno, off_t> seek(const CurrentTask& current_task, SeekTarget target) const;

 private:
  FileObject(WeakFileHandle weak_handle, FileObjectId id, NamespaceNode name, FileSystemHandle fs,
             ktl::unique_ptr<FileOps> ops, OpenFlags flags);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_OBJECT_H_
