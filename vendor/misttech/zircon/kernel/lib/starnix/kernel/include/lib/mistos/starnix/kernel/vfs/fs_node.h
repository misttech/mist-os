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
  // link_behavior: OnceCell<FsNodeLinkBehavior>,

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

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(const CurrentTask& current_task,
                                                               OpenFlags flags) const;

  fit::result<Errno, ktl::unique_ptr<FileOps>> open(const CurrentTask& current_task,
                                                    const MountInfo& mount, OpenFlags flags,
                                                    bool check_access) const;

  fit::result<Errno, FsNodeHandle> lookup(const CurrentTask& current_task, const MountInfo& mount,
                                          const FsStr& name) const;

  fit::result<Errno, FsNodeHandle> mknod(const CurrentTask& current_task, const MountInfo& mount,
                                         const FsStr& name, FileMode mode, DeviceType dev,
                                         FsCred owner) const;

  /// This method does not attempt to update the atime of the node.
  /// Use `NamespaceNode::readlink` which checks the mount flags and updates the atime accordingly.
  fit::result<Errno, SymlinkTarget> readlink(const CurrentTask& current_task) const;

  static fit::result<Errno> default_check_access_impl(
      const CurrentTask& current_task, Access access,
      starnix_sync::RwLockGuard<FsNodeInfo, BrwLockPi::Reader> info);

  /// Check whether the node can be accessed in the current context with the specified access
  /// flags (read, write, or exec). Accounts for capabilities and whether the current user is the
  /// owner or is in the file's group.
  fit::result<Errno> check_access(const CurrentTask& current_task, const MountInfo& mount,
                                  Access access, CheckAccessReason reason) const;

  /// Whether this node is a regular file.
  bool is_reg() const { return info()->mode_.is_reg(); }

  /// Whether this node is a directory.
  bool is_dir() const { return info()->mode_.is_dir(); }

  /// Whether this node is a symbolic link.
  bool is_lnk() const { return info()->mode_.is_lnk(); }

  fit::result<Errno, struct ::stat> stat(const CurrentTask& current_task) const;

  // Returns current `FsNodeInfo`.
  starnix_sync::RwLockGuard<FsNodeInfo, BrwLockPi::Reader> info() const { return info_.Read(); }

  /// Refreshes the `FsNodeInfo` if necessary and returns a read lock.
  fit::result<Errno, FsNodeInfo> fetch_and_refresh_info(const CurrentTask& current_task) const;

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

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_H_
