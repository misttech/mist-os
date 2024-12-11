// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_OPS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_OPS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/falloc.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_info.h>
#include <lib/mistos/starnix/kernel/vfs/namespace_node.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/error_propagation.h>
#include <lib/starnix_sync/locks.h>

#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>
#include <ktl/variant.h>

#include <asm/stat.h>
#include <linux/fsverity.h>
#include <linux/xattr.h>

namespace starnix {

using starnix_uapi::DeviceType;
using starnix_uapi::Errno;
using starnix_uapi::FileMode;
using starnix_uapi::FsCred;
using starnix_uapi::OpenFlags;

class CurrentTask;
class FileOps;
class FsNode;
using FsNodeHandle = fbl::RefPtr<FsNode>;
class SymlinkTarget;

enum class XattrOp : uint8_t {
  Set,
  Create,
  Replace,
};

class XattrOpHelper {
 public:
  static uint32_t into_flags(XattrOp op) {
    switch (op) {
      case XattrOp::Set:
        return 0;
      case XattrOp::Create:
        return XATTR_CREATE;
      case XattrOp::Replace:
        return XATTR_REPLACE;
    }
    return 0;  // Default to 0 if op is not recognized
  }
};

/// Returns a value, or the size required to contains it.
template <typename T>
class ValueOrSize {
 public:
  using Variant = ktl::variant<T, size_t>;

  static ValueOrSize Value(T val) { return ValueOrSize(val); }
  static ValueOrSize Size(size_t size) { return ValueOrSize(size); }

  explicit ValueOrSize(Variant variant) : variant_(variant) {}

 private:
  Variant variant_;
};

struct zxio_node_attr_has_t {
  bool protocols_;
  bool abilities_;
  bool id_;
  bool content_size_;
  bool storage_size_;
  bool link_count_;
  bool creation_time_;
  bool modification_time_;
  bool change_time_;
  bool access_time_;
  bool mode_;
  bool uid_;
  bool gid_;
  bool rdev_;
  bool fsverity_options_;
  bool fsverity_root_hash_;
  bool fsverity_enabled_;
  bool object_type_;
  bool casefold_;
  bool wrapping_key_id_;
  bool selinux_context_;
};

class FsNodeOps {
 public:
  /// Delegate the access check to the node.
  virtual fit::result<Errno> check_access(const FsNode& node, const CurrentTask& current_task,
                                          starnix_uapi::Access access,
                                          starnix_sync::RwLock<FsNodeInfo>& info,
                                          CheckAccessReason _reason) const;

  /// Build the [`DirEntryOps`] for a new [`DirEntry`] that will be associated
  /// to this node.
  // virtual ktl::unique_ptr<DirEntryOps> create_dir_entry_ops() const {
  //   return ktl::make_unique<DefaultDirEntryOps>();
  // }

  /// Build the `FileOps` for the file associated to this node.
  ///
  /// The returned FileOps will be used to create a FileObject, which might
  /// be assigned an FdNumber.
  virtual fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(
      const FsNode& node, const CurrentTask& current_task, OpenFlags flags) const = 0;

  /// Find an existing child node and populate the child parameter. Return the node.
  ///
  /// The child parameter is an empty node. Operations other than initialize may panic before
  /// initialize is called.
  virtual fit::result<Errno, FsNodeHandle> lookup(const FsNode& node,
                                                  const CurrentTask& current_task,
                                                  const FsStr& name) const;

  /// Create and return the given child node.
  ///
  /// The mode field of the FsNodeInfo indicates what kind of child to
  /// create.
  ///
  /// This function is never called with FileMode::IFDIR. The mkdir function
  /// is used to create directories instead.
  virtual fit::result<Errno, FsNodeHandle> mknod(const FsNode& node,
                                                 const CurrentTask& current_task, const FsStr& name,
                                                 FileMode mode, DeviceType dev,
                                                 FsCred owner) const = 0;

  /// Create and return the given child node as a subdirectory.
  virtual fit::result<Errno, FsNodeHandle> mkdir(const FsNode& node,
                                                 const CurrentTask& current_task, const FsStr& name,
                                                 FileMode mode, FsCred owner) const = 0;

  /// Creates a symlink with the given `target` path.
  virtual fit::result<Errno, FsNodeHandle> create_symlink(const FsNode& node,
                                                          const CurrentTask& current_task,
                                                          const FsStr& name, const FsStr& target,
                                                          FsCred owner) const = 0;

  /// Creates an anonymous file.
  ///
  /// The FileMode::IFMT of the FileMode is always FileMode::IFREG.
  ///
  /// Used by O_TMPFILE.
  virtual fit::result<Errno, FsNodeHandle> create_tmpfile(const FsNode& node,
                                                          const CurrentTask& current_task,
                                                          FileMode mode, FsCred owner) const;

  /// Reads the symlink from this node.
  virtual fit::result<Errno, SymlinkTarget> readlink(const FsNode& node,
                                                     const CurrentTask& current_task) const {
    return fit::error(errno(EINVAL));
  }

  /// Create a hard link with the given name to the given child.
  virtual fit::result<Errno> link(const FsNode& node, const CurrentTask& current_task,
                                  const FsStr& name, const FsNodeHandle& child) const {
    return fit::error(errno(EPERM));
  }

  /// Remove the child with the given name, if the child exists.
  ///
  /// The UnlinkKind parameter indicates whether the caller intends to unlink
  /// a directory or a non-directory child.
  virtual fit::result<Errno> unlink(const FsNode& node, const CurrentTask& current_task,
                                    const FsStr& name, const FsNodeHandle& child) const = 0;

  /// Change the length of the file.
  virtual fit::result<Errno> truncate(const FsNode& node, const CurrentTask& current_task,
                                      uint64_t length) const {
    return fit::error(errno(EINVAL));
  }

  /// Manipulate allocated disk space for the file.
  virtual fit::result<Errno> allocate(const FsNode& node, const CurrentTask& current_task,
                                      FallocMode mode, uint64_t offset, uint64_t length) const {
    return fit::error(errno(EINVAL));
  }

  /// Update the supplied info with initial state (e.g. size) for the node.
  ///
  /// FsNode calls this method when created, to allow the FsNodeOps to
  /// set appropriate initial values in the FsNodeInfo.
  virtual void initial_info(FsNodeInfo& info) const {}

  /// Update node.info as needed.
  ///
  /// FsNode calls this method before converting the FsNodeInfo struct into
  /// the uapi::stat struct to give the file system a chance to update this data
  /// before it is used by clients.
  ///
  /// File systems that keep the FsNodeInfo up-to-date do not need to
  /// override this function.
  ///
  /// Return a reader lock on the updated information.
  virtual fit::result<Errno, FsNodeInfo> fetch_and_refresh_info(
      const FsNode& node, const CurrentTask& current_task,
      starnix_sync::RwLock<FsNodeInfo>& info) const {
    return fit::ok(*info.Read());
  }

  /// Indicates if the filesystem can manage the timestamps (i.e. atime, ctime, and mtime).
  ///
  /// Starnix updates the timestamps in node.info directly. However, if the filesystem can manage
  /// the timestamps, then Starnix does not need to do so. `node.info`` will be refreshed with the
  /// timestamps from the filesystem by calling `refresh_info(..)`.
  virtual bool filesystem_manages_timestamps(const FsNode& node) const { return false; }

  /// Update node attributes persistently.
  virtual fit::result<Errno> update_attributes(const CurrentTask& current_task,
                                               const FsNodeInfo& info,
                                               zxio_node_attr_has_t has) const {
    return fit::ok();
  }

  /// Get an extended attribute on the node.
  ///
  /// An implementation can systematically return a value. Otherwise, if `max_size` is 0, it can
  /// instead return the size of the attribute, and can return an ERANGE error if max_size is not
  /// 0, and lesser than the required size.
  virtual fit::result<Errno, ValueOrSize<FsString>> get_xattr(const FsNode& node,
                                                              const CurrentTask& current_task,
                                                              const FsStr& name,
                                                              size_t max_size) const {
    return fit::error(errno(ENOTSUP));
  }

  /// Set an extended attribute on the node.
  virtual fit::result<Errno> set_xattr(const FsNode& node, const CurrentTask& current_task,
                                       const FsStr& name, const FsStr& value, XattrOp op) const {
    return fit::error(errno(ENOTSUP));
  }

  virtual fit::result<Errno> remove_xattr(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name) const {
    return fit::error(errno(ENOTSUP));
  }

  /// An implementation can systematically return a value. Otherwise, if `max_size` is 0, it can
  /// instead return the size of the 0 separated string needed to represent the value, and can
  /// return an ERANGE error if max_size is not 0, and lesser than the required size.
  virtual fit::result<Errno, ValueOrSize<fbl::Vector<FsString>>> list_xattrs(
      const FsNode& node, const CurrentTask& current_task, size_t max_size) const {
    return fit::error(errno(ENOTSUP));
  }

  /// Called when the FsNode is freed by the Kernel.
  virtual fit::result<Errno> forget(const FsNode& node, const CurrentTask& current_task) const {
    return fit::ok();
  }

  ////////////////////
  // FS-Verity operations

  /// Marks that FS-Verity is being built. Writes fsverity descriptor and merkle tree, the
  /// latter computed by the filesystem. This should ensure there are no writable file handles.
  /// Returns EEXIST if the file was already fsverity-enabled. Returns EBUSY if this ioctl was
  /// already running on this file.
  virtual fit::result<Errno> enable_fsverity(const fsverity_descriptor& descriptor) const {
    return fit::error(errno(ENOTSUP));
  }

  /// Read fsverity descriptor, if the node is fsverity-enabled. Else returns ENODATA.
  virtual fit::result<Errno, fsverity_descriptor> get_fsverity_descriptor(
      uint8_t log_blocksize) const {
    return fit::error(errno(ENOTSUP));
  }

  // C++
  virtual ~FsNodeOps();
};

/// Implements [`FsNodeOps`] methods in a way that makes sense for symlinks.
/// You must implement [`FsNodeOps::readlink`].
#define fs_node_impl_symlink                                                              \
  fs_node_impl_not_dir();                                                                 \
                                                                                          \
  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(                           \
      const FsNode& node, const CurrentTask& current_task, OpenFlags flags) const final { \
    ZX_ASSERT(node.is_lnk());                                                             \
    panic("Symlink nodes cannot be opened.");                                             \
  }                                                                                       \
  using __fs_node_impl_symlink_force_semicolon = int

#define fs_node_impl_dir_readonly                                                                  \
  fit::result<Errno, FsNodeHandle> mkdir(const FsNode& node, const CurrentTask& current_task,      \
                                         const FsStr& name, FileMode mode, FsCred owner)           \
      const final {                                                                                \
    return fit::error(errno(EROFS));                                                               \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, FsNodeHandle> mknod(const FsNode& node, const CurrentTask& current_task,      \
                                         const FsStr& name, FileMode mode, DeviceType dev,         \
                                         FsCred owner) const final {                               \
    return fit::error(errno(EROFS));                                                               \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, FsNodeHandle> create_symlink(                                                 \
      const FsNode& node, const CurrentTask& current_task, const FsStr& name, const FsStr& target, \
      FsCred owner) const final {                                                                  \
    return fit::error(errno(EROFS));                                                               \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno> link(const FsNode& node, const CurrentTask& current_task, const FsStr& name,  \
                          const FsNodeHandle& child) const final {                                 \
    return fit::error(errno(EROFS));                                                               \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno> unlink(const FsNode& node, const CurrentTask& current_task,                   \
                            const FsStr& name, const FsNodeHandle& child) const final {            \
    return fit::error(errno(EROFS));                                                               \
  }                                                                                                \
  using __fs_node_impl_dir_readonly_force_semicolon = int

/// Trait that objects can implement if they need to handle extended attribute storage. Allows
/// delegating extended attribute operations in [`FsNodeOps`] to another object.
///
/// See [`fs_node_impl_xattr_delegate`] for usage details.
class XattrStorage {
 public:
  /// Delegate for [`FsNodeOps::get_xattr`].
  virtual fit::result<Errno, FsString> get_xattr(const FsStr& name) const = 0;

  /// Delegate for [`FsNodeOps::set_xattr`].
  virtual fit::result<Errno> set_xattr(const FsStr& name, const FsStr& value, XattrOp op) const = 0;

  /// Delegate for [`FsNodeOps::remove_xattr`].
  virtual fit::result<Errno> remove_xattr(const FsStr& name) const = 0;

  /// Delegate for [`FsNodeOps::list_xattrs`].
  virtual fit::result<Errno, fbl::Vector<FsString>> list_xattrs(const FsStr& name) const = 0;

  virtual ~XattrStorage();
};

/// Implements extended attribute ops for [`FsNodeOps`] by delegating to another object which
/// implements the [`XattrStorage`] trait or a similar interface. For example:
///
/// ```
/// struct Xattrs {}
///
/// impl XattrStorage for Xattrs {
///     // implement XattrStorage
/// }
///
/// struct Node {
///     xattrs: Xattrs
/// }
///
/// impl FsNodeOps for Node {
///     // Delegate extended attribute ops in FsNodeOps to self.xattrs
///     fs_node_impl_xattr_delegate!(self, self.xattrs);
///
///     // add other FsNodeOps impls here
/// }
/// ```
#define fs_node_impl_xattr_delegate(delegate)                                                   \
  fit::result<Errno, ValueOrSize<FsString>> get_xattr(                                          \
      const FsNode& node, const CurrentTask& current_task, const FsStr& name, size_t max_size)  \
      const final {                                                                             \
    auto get_xattr_result = (delegate).get_xattr(name) _EP(get_xattr_result);                   \
    return fit::ok(get_xattr_result.value());                                                   \
  }                                                                                             \
                                                                                                \
  fit::result<Errno> set_xattr(const FsNode& node, const CurrentTask& current_task,             \
                               const FsStr& name, const FsStr& value, XattrOp op) const final { \
    return (delegate).set_xattr(name, value, op);                                               \
  }                                                                                             \
                                                                                                \
  fit::result<Errno> remove_xattr(const FsNode& node, const CurrentTask& current_task,          \
                                  const FsStr& name) const final {                              \
    return (delegate).remove_xattr(name);                                                       \
  }                                                                                             \
                                                                                                \
  fit::result<Errno, ValueOrSize<fbl::Vector<FsString>>> list_xattrs(                           \
      const FsNode& node, const CurrentTask& current_task, size_t max_size) const final {       \
    return fit::error(errno(ENOTSUP));                                                          \
  }                                                                                             \
  using __fs_node_impl_xattr_delegate_force_semicolon = int

/// Stubs out [`FsNodeOps`] methods that only apply to directories.
#define fs_node_impl_not_dir                                                                       \
  fit::result<Errno, FsNodeHandle> lookup(const FsNode& node, const CurrentTask& current_task,     \
                                          const FsStr& name) const final {                         \
    return fit::error(errno(ENOTDIR));                                                             \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, FsNodeHandle> mknod(const FsNode& node, const CurrentTask& current_task,      \
                                         const FsStr& name, FileMode mode, DeviceType dev,         \
                                         FsCred owner) const final {                               \
    return fit::error(errno(ENOTDIR));                                                             \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, FsNodeHandle> mkdir(const FsNode& node, const CurrentTask& current_task,      \
                                         const FsStr& name, FileMode mode, FsCred owner)           \
      const final {                                                                                \
    return fit::error(errno(ENOTDIR));                                                             \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, FsNodeHandle> create_symlink(                                                 \
      const FsNode& node, const CurrentTask& current_task, const FsStr& name, const FsStr& target, \
      FsCred owner) const final {                                                                  \
    return fit::error(errno(ENOTDIR));                                                             \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno> unlink(const FsNode& node, const CurrentTask& current_task,                   \
                            const FsStr& name, const FsNodeHandle& child) const final {            \
    return fit::error(errno(ENOTDIR));                                                             \
  }                                                                                                \
  using __fs_node_impl_not_dir_force_semicolon = int

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_OPS_H_
