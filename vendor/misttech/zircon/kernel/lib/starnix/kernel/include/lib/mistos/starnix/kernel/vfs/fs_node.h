// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/memory/weak_ptr.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_info.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix/kernel/vfs/pipe.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/oncelock.h>
#include <lib/starnix_sync/locks.h>
#include <zircon/compiler.h>

#include <fbl/intrusive_single_list.h>
#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <ktl/unique_ptr.h>
#include <ktl/variant.h>

#include <asm/stat.h>

namespace starnix {

class Kernel;
class CurrentTask;
using PipeHandle = fbl::RefPtr<Pipe>;
using starnix_uapi::Credentials;

// Whether linking is allowed for this node
enum class FsNodeLinkBehavior : uint8_t { kAllowed, kDisallowed };

inline FsNodeLinkBehavior DefaultFsNodeLinkBehavior() { return FsNodeLinkBehavior::kAllowed; }

// The inner class is required because bitflags cannot pass the attribute through to the single
// variant, and attributes cannot be applied to macro invocations.
namespace inner_flags {
enum class StatxFlagsEnum : uint32_t {
  _AT_SYMLINK_NOFOLLOW = AT_SYMLINK_NOFOLLOW,
  _AT_EMPTY_PATH = AT_EMPTY_PATH,
  _AT_NO_AUTOMOUNT = AT_NO_AUTOMOUNT,
  _AT_STATX_SYNC_AS_STAT = AT_STATX_SYNC_AS_STAT,
  _AT_STATX_FORCE_SYNC = AT_STATX_FORCE_SYNC,
  _AT_STATX_DONT_SYNC = AT_STATX_DONT_SYNC,
  _STATX_ATTR_VERITY = STATX_ATTR_VERITY,
};

using StatxFlags = Flags<StatxFlagsEnum>;
}  // namespace inner_flags

using StatxFlags = inner_flags::StatxFlags;
using StatxFlagsEnum = inner_flags::StatxFlagsEnum;

class FsNode final : public fbl::SinglyLinkedListable<mtl::WeakPtr<FsNode>>,
                     public fbl::RefCountedUpgradeable<FsNode> {
 public:
  /// Weak reference to the `FsNodeHandle` of this `FsNode`. This allows to retrieve the
  /// `FsNodeHandle` from a `FsNode`.
  WeakFsNodeHandle weak_handle_;

 private:
  /// The FsNodeOps for this FsNode.
  ///
  /// The FsNodeOps are implemented by the individual file systems to provide
  /// specific behaviors for this FsNode.
  ktl::unique_ptr<FsNodeOps> ops_;

  /// The current kernel.
  // TODO(https://fxbug.dev/42080557): This is a temporary measure to access a task on drop.
  mtl::WeakPtr<Kernel> kernel_;

  /// The FileSystem that owns this FsNode's tree.
  mtl::WeakPtr<FileSystem> fs_;

 public:
  // The node idenfier for this FsNode. By default, this will be used as the inode number of
  // this node.
  ino_t node_id_ = 0;

  /// The pipe located at this node, if any.
  ///
  /// Used if, and only if, the node has a mode of FileMode::IFIFO.
  // pub fifo: Option<PipeHandle>,
  ktl::optional<PipeHandle> fifo_;

 private:
  /// The socket located at this node, if any.
  ///
  /// Used if, and only if, the node has a mode of FileMode::IFSOCK.
  ///
  /// The `OnceCell` is initialized when a new socket node is created:
  ///   - in `Socket::new` (e.g., from `sys_socket`)
  ///   - in `sys_bind`, before the node is given a name (i.e., before it could be accessed by
  ///     others)
  // socket: OnceCell<SocketHandle>,

 public:
  /// A RwLock to synchronize append operations for this node.
  ///
  /// FileObjects writing with O_APPEND should grab a write() lock on this
  /// field to ensure they operate sequentially. FileObjects writing without
  /// O_APPEND should grab read() lock so that they can operate in parallel.
  // pub append_lock: RwQueue,

 private:
  /// Mutable information about this node.
  ///
  /// This data is used to populate the uapi::stat structure.
  mutable starnix_sync::RwLock<FsNodeInfo> info_;

  /// Information about the locking information on this node.
  ///
  /// No other lock on this object may be taken while this lock is held.
  // flock_info: Mutex<FlockInfo>,

  /// Records locks associated with this node.
  // record_locks: RecordLocks,

  /// Whether this node can be linked into a directory.
  ///
  /// Only set for nodes created with `O_TMPFILE`.
  mtl::OnceLock<FsNodeLinkBehavior> link_behavior_;

  /// Tracks lock state for this file.
  // pub write_guard_state: Mutex<FileWriteGuardState>,

  /// Cached Fsverity state associated with this node.
  // pub fsverity: Mutex<FsVerityState>,

  /// Inotify watchers on this node. See inotify(7).
  // pub watchers: inotify::InotifyWatchers,

  /// impl FsNode
 public:
  // Create a new node with default value for the root of a filesystem.
  ///
  /// The node identifier and ino will be set by the filesystem on insertion. It will be owned by
  /// root and have a 777 permission.
  static FsNode* new_root(FsNodeOps* ops);

  // Create a new node for the root of a filesystem.
  ///
  /// The provided callback allows the caller to set the properties of the node.
  /// The default value will provided a node owned by root, with permission 0777.
  /// The ino will be 0. If left as is, it will be set by the filesystem on insertion.
  template <class F>
  static FsNode* new_root_with_properties(FsNodeOps* ops, F&& info_updater) {
    auto info = FsNodeInfo::New(0, FILE_MODE(IFDIR, 0777), FsCred::root());
    info_updater(info);
    return FsNode::new_internal(ktl::unique_ptr<FsNodeOps>(ops), mtl::WeakPtr<Kernel>(),
                                mtl::WeakPtr<FileSystem>(), 0, info, Credentials::root());
  }

  /// Create a node without inserting it into the FileSystem node cache. This is usually not what
  /// you want! Only use if you're also using get_or_create_node, like ext4.
  static FsNodeHandle new_uncached(const CurrentTask& current_task, ktl::unique_ptr<FsNodeOps> ops,
                                   const FileSystemHandle& fs, ino_t node_id, FsNodeInfo info);

  // Used by BootFS (no current task available)
  static FsNodeHandle new_uncached_with_creds(ktl::unique_ptr<FsNodeOps> ops,
                                              const FileSystemHandle& fs, ino_t node_id,
                                              FsNodeInfo info, const Credentials& credentials);

  FsNodeHandle into_handle() {
    FsNodeHandle handle = fbl::AdoptRef(this);
    weak_handle_ = handle->weak_factory_.GetWeakPtr();
    return ktl::move(handle);
  }

 private:
  static FsNode* new_internal(ktl::unique_ptr<FsNodeOps> ops, mtl::WeakPtr<Kernel> kernel,
                              mtl::WeakPtr<FileSystem> fs, ino_t node_id, FsNodeInfo info,
                              const Credentials& credentials);

 public:
  void set_id(ino_t node_id) {
    DEBUG_ASSERT(node_id_ == 0);
    node_id_ = node_id;
    /*
      if self.info.get_mut().ino == 0 {
          self.info.get_mut().ino = node_id;
      }
    */
  }

  FileSystemHandle fs() const {
    auto fs = fs_.Lock();
    ASSERT_MSG(fs, "FileSystem did not live long enough");
    return fs;
  }

  void set_fs(const FileSystemHandle& fs) {
    fs_ = fs->weak_factory_.GetWeakPtr();
    kernel_ = fs->kernel_;
  }

  FsNodeOps& ops() const { return *ops_; }

  template <typename T>
  ktl::optional<T*> downcast_ops() const {
    auto ptr = static_cast<T*>(ops_.get());
    return ptr ? ktl::optional<T*>(ptr) : ktl::nullopt;
  }

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(const CurrentTask& current_task,
                                                               OpenFlags flags) const;

  fit::result<Errno, ktl::unique_ptr<FileOps>> open(const CurrentTask& current_task,
                                                    const MountInfo& mount, OpenFlags flags,
                                                    AccessCheck access_check) const;

  fit::result<Errno, FsNodeHandle> lookup(const CurrentTask& current_task, const MountInfo& mount,
                                          const FsStr& name) const;

  fit::result<Errno, FsNodeHandle> mknod(const CurrentTask& current_task, const MountInfo& mount,
                                         const FsStr& name, FileMode mode, DeviceType dev,
                                         FsCred owner) const;

  fit::result<Errno, FsNodeHandle> create_symlink(const CurrentTask& current_task,
                                                  const MountInfo& mount, const FsStr& name,
                                                  const FsStr& target, FsCred owner) const;

  /// This method does not attempt to update the atime of the node.
  /// Use `NamespaceNode::readlink` which checks the mount flags and updates the atime accordingly.
  fit::result<Errno, SymlinkTarget> readlink(const CurrentTask& current_task) const;

  fit::result<Errno, FsNodeHandle> link(const CurrentTask& current_task, const MountInfo& mount,
                                        const FsStr& name, const FsNodeHandle& child) const;

  fit::result<Errno> unlink(const CurrentTask& current_task, const MountInfo& mount,
                            const FsStr& name, const FsNodeHandle& child) const;

  static fit::result<Errno> default_check_access_impl(
      const CurrentTask& current_task, Access access,
      starnix_sync::RwLockGuard<FsNodeInfo, BrwLockPi::Reader> info);

  /// Check whether the node can be accessed in the current context with the specified access
  /// flags (read, write, or exec). Accounts for capabilities and whether the current user is the
  /// owner or is in the file's group.
  fit::result<Errno> check_access(const CurrentTask& current_task, const MountInfo& mount,
                                  Access access, CheckAccessReason reason) const;

  /// Check whether the stick bit, `S_ISVTX`, forbids the `current_task` from removing the given
  /// `child`. If this node has `S_ISVTX`, then either the child must be owned by the `fsuid` of
  /// `current_task` or `current_task` must have `CAP_FOWNER`.
  fit::result<Errno> check_sticky_bit(const CurrentTask& current_task,
                                      const FsNodeHandle& child) const;

  template <typename Fn>
  fit::result<Errno> update_attributes(const CurrentTask& current_task, Fn&& mutator) const {
    auto info = info_.Write();
    auto new_info = *info;
    _EP(mutator(new_info));

    zxio_node_attr_has_t has;
    has.modification_time_ = false;  // info->time_modify_ != new_info.time_modify_;
    has.access_time_ = false;        // info->time_access_ != new_info.time_access_;
    has.mode_ = info->mode_ != new_info.mode_;
    has.uid_ = info->uid_ != new_info.uid_;
    has.gid_ = info->gid_ != new_info.gid_;
    has.rdev_ = info->rdev_ != new_info.rdev_;
    has.casefold_ = false;  // info->casefold_ != new_info.casefold_;

    // Call `update_attributes(..)` to persist the changes for the following fields.
    if (has.modification_time_ || has.access_time_ || has.mode_ || has.uid_ || has.gid_ ||
        has.rdev_ || has.casefold_) {
      _EP(ops_->update_attributes(current_task, new_info, has));
    }

    *info = new_info;
    return fit::ok();
  }

  /// Set the permissions on this FsNode to the given values.
  ///
  /// Does not change the IFMT of the node.
  fit::result<Errno> chmod(const CurrentTask& current_task, const MountInfo& mount,
                           FileMode mode) const;

  /// Whether this node is a regular file.
  bool is_reg() const { return info()->mode_.is_reg(); }

  /// Whether this node is a directory.
  bool is_dir() const { return info()->mode_.is_dir(); }

  /// Whether this node is a symbolic link.
  bool is_lnk() const { return info()->mode_.is_lnk(); }

  fit::result<Errno, struct ::stat> stat(const CurrentTask& current_task) const;

  fit::result<Errno, struct ::statx> statx(const CurrentTask& current_task, StatxFlags flags,
                                           uint32_t mask) const;

  // Returns current `FsNodeInfo`.
  starnix_sync::RwLock<FsNodeInfo>::RwLockReadGuard info() const { return info_.Read(); }

  /// Refreshes the `FsNodeInfo` if necessary and returns a read lock.
  fit::result<Errno, starnix_sync::RwLock<FsNodeInfo>::RwLockReadGuard> fetch_and_refresh_info(
      const CurrentTask& current_task) const;

  template <typename T, typename F>
  T update_info(F&& mutator) const {
    auto info = info_.Write();
    return mutator(*info);
  }

  // impl Releasable for FsNode
  // Release code is implemented in the dtcor.

  // C++

  // Required to instantiate fbl::DefaultKeyedObjectTraits.
  ino_t GetKey() const { return node_id_; }

  // Required to instantiate fbl::DefaultHashTraits.
  static size_t GetHash(ino_t key) { return key; }

  ~FsNode();

 private:
  DISALLOW_COPY_ASSIGN_AND_MOVE(FsNode);

  FsNode(WeakFsNodeHandle weak_handle, mtl::WeakPtr<Kernel> kernel, ktl::unique_ptr<FsNodeOps> ops,
         mtl::WeakPtr<FileSystem> fs, ino_t node_id, ktl::optional<PipeHandle>, FsNodeInfo info);

 public:
  mtl::WeakPtrFactory<FsNode> weak_factory_;  // must be last
};

}  // namespace starnix

template <>
constexpr Flag<starnix::inner_flags::StatxFlagsEnum>
    Flags<starnix::inner_flags::StatxFlagsEnum>::FLAGS[] = {
        {starnix::inner_flags::StatxFlagsEnum::_AT_SYMLINK_NOFOLLOW},
        {starnix::inner_flags::StatxFlagsEnum::_AT_EMPTY_PATH},
        {starnix::inner_flags::StatxFlagsEnum::_AT_NO_AUTOMOUNT},
        {starnix::inner_flags::StatxFlagsEnum::_AT_STATX_SYNC_AS_STAT},
        {starnix::inner_flags::StatxFlagsEnum::_AT_STATX_FORCE_SYNC},
        {starnix::inner_flags::StatxFlagsEnum::_AT_STATX_DONT_SYNC},
        {starnix::inner_flags::StatxFlagsEnum::_STATX_ATTR_VERITY},
};

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_H_
