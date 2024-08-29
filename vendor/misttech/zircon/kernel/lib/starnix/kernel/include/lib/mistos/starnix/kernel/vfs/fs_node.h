// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/sync/locks.h>
#include <lib/mistos/starnix/kernel/task/forward.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/forward.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix/kernel/vfs/pipe.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <zircon/compiler.h>

#include <functional>

#include <fbl/intrusive_single_list.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <ktl/unique_ptr.h>
#include <ktl/variant.h>

#include <asm/stat.h>
#include <linux/falloc.h>
#include <linux/fsverity.h>
#include <linux/xattr.h>

namespace starnix {

using namespace starnix_uapi;

/// `st_blksize` is measured in units of 512 bytes.
const size_t DEFAULT_BYTES_PER_BLOCK = 512;

struct FsNodeInfo {
  ino_t ino;
  FileMode mode;
  size_t link_count;
  uid_t uid;
  gid_t gid;
  DeviceType rdev;
  size_t size;
  size_t blksize;
  size_t blocks;
  // pub time_status_change: zx::Time,
  // pub time_access: zx::Time,
  // pub time_modify: zx::Time,
  // pub sid: Option<SecurityId>,

  /// impl FsNodeInfo
  static FsNodeInfo New(ino_t ino, FileMode mode, FsCred owner) {
    return {
        .ino = ino,
        .mode = mode,
        .uid = owner.uid,
        .gid = owner.gid,
        .rdev = DeviceType(0),
        .size = 0,
        .blksize = DEFAULT_BYTES_PER_BLOCK,
        .blocks = 0,
    };
  }

  size_t storage_size() const {
    // TODO (Herrera) : saturating_mul
    return blksize * blocks;
  }

  static std::function<FsNodeInfo(ino_t)> new_factory(FileMode mode, FsCred owner) {
    return [mode, owner](ino_t ino) -> FsNodeInfo { return FsNodeInfo::New(ino, mode, owner); };
  }

  void chmod(const FileMode& m) {
    mode = (mode & !FileMode::PERMISSIONS) | (m & FileMode::PERMISSIONS);
    // self.time_status_change = utc::utc_now();
  }
};

class FsNode final
    : public fbl::SinglyLinkedListable<util::WeakPtr<FsNode>, fbl::NodeOptions::AllowClearUnsafe>,
      private fbl::RefCountedUpgradeable<FsNode> {
 public:
  /// Weak reference to the `FsNodeHandle` of this `FsNode`. This allows to retrieve the
  /// `FsNodeHandle` from a `FsNode`.
  WeakFsNodeHandle weak_handle;

 private:
  /// The FsNodeOps for this FsNode.
  ///
  /// The FsNodeOps are implemented by the individual file systems to provide
  /// specific behaviors for this FsNode.
  ktl::unique_ptr<FsNodeOps> ops_;

  /// The current kernel.
  // TODO(https://fxbug.dev/42080557): This is a temporary measure to access a task on drop.
  // kernel: Weak<Kernel>,
  util::WeakPtr<Kernel> kernel_;

  /// The FileSystem that owns this FsNode's tree.
  // fs: Weak<FileSystem>,
  util::WeakPtr<FileSystem> fs_;

 public:
  // The node idenfier for this FsNode. By default, this will be used as the inode number of
  // this node.
  ino_t node_id = 0;

  /// The pipe located at this node, if any.
  ///
  /// Used if, and only if, the node has a mode of FileMode::IFIFO.
  // pub fifo: Option<PipeHandle>,
  ktl::optional<PipeHandle> fifo;

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
  mutable RwLock<FsNodeInfo> info_;

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
  static FsNode* new_root_with_properties(FsNodeOps* ops, F info_updater) {
    auto info = FsNodeInfo::New(0, FILE_MODE(IFDIR, 0777), FsCred::root());
    info_updater(info);
    return FsNode::new_internal(ktl::unique_ptr<FsNodeOps>(ops), util::WeakPtr<Kernel>(),
                                util::WeakPtr<FileSystem>(), 0, info, Credentials::root());
  }

  /// Create a node without inserting it into the FileSystem node cache. This is usually not what
  /// you want! Only use if you're also using get_or_create_node, like ext4.
  static FsNodeHandle new_uncached(const CurrentTask& current_task, ktl::unique_ptr<FsNodeOps> ops,
                                   const FileSystemHandle& fs, ino_t node_id, FsNodeInfo info) {
    // auto creds = current_task.creds();
    return FsNode::new_internal(std::move(ops), fs->kernel(), util::WeakPtr(fs.get()), node_id,
                                info, Credentials::root())
        ->into_handle();
  }

  FsNodeHandle into_handle() {
    FsNodeHandle handle = fbl::AdoptRef(this);
    weak_handle = util::WeakPtr<FsNode>(handle.get());
    return std::move(handle);
  }

 private:
  static FsNode* new_internal(ktl::unique_ptr<FsNodeOps> ops, util::WeakPtr<Kernel> kernel,
                              util::WeakPtr<FileSystem> fs, ino_t node_id, FsNodeInfo info,
                              const Credentials& credentials);

 public:
  void set_id(ino_t _node_id) {
    DEBUG_ASSERT(node_id == 0);
    node_id = _node_id;
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
    fs_ = util::WeakPtr<FileSystem>(fs.get());
    kernel_ = fs->kernel();
  }

  FsNodeOps& ops() const { return *ops_.get(); }

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

  // Whether this node is a directory.
  bool is_dir() const { return info()->mode.is_dir(); }

  /// Whether this node is a symbolic link.
  bool is_lnk() const { return info()->mode.is_lnk(); }

  fit::result<Errno, struct stat> stat(const CurrentTask& current_task) const;

  // Returns current `FsNodeInfo`.
  RwLockGuard<FsNodeInfo, BrwLockPi::Reader> info() const { return info_.Read(); }

  /// Refreshes the `FsNodeInfo` if necessary and returns a read lock.
  fit::result<Errno, FsNodeInfo> refresh_info(const CurrentTask& current_task) const;

  template <typename T>
  T update_info(std::function<T(FsNodeInfo&)> mutator) const {
    auto _info = info_.Write();
    return mutator(*_info);
  }

 public:
  // C++
  using fbl::RefCountedUpgradeable<FsNode>::AddRef;
  using fbl::RefCountedUpgradeable<FsNode>::Release;
  using fbl::RefCountedUpgradeable<FsNode>::Adopt;
  using fbl::RefCountedUpgradeable<FsNode>::AddRefMaybeInDestructor;

  // Required to instantiate fbl::DefaultKeyedObjectTraits.
  ino_t GetKey() const { return node_id; }

  // Required to instantiate fbl::DefaultHashTraits.
  static size_t GetHash(ino_t key) { return key; }

 private:
  FsNode(WeakFsNodeHandle weak_handle, util::WeakPtr<Kernel> kernel, ktl::unique_ptr<FsNodeOps> ops,
         util::WeakPtr<FileSystem> fs, ino_t node_id, ktl::optional<PipeHandle>, FsNodeInfo info);
};

enum class XattrOp {
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

struct SymlinkTarget {
  SymlinkTarget(const FsString& path) : value(path) {}
  SymlinkTarget(NamespaceNode node) : value(node) {}

  // Helpers from the reference documentation for std::visit<>, to allow
  // visit-by-overload of the std::variant<>:
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  std::variant<FsString, NamespaceNode> value;
};

/// Returns a value, or the size required to contains it.
template <typename T>
class ValueOrSize {
 public:
  ValueOrSize(T val) : value(val) {}
  ValueOrSize(size_t size) : value(size) {}

 private:
  std::variant<T, size_t> value;
};

enum class FallocMode {
  Allocate,
  PunchHole,
  Collapse,
  Zero,
  InsertRange,
  UnshareRange,
};

class FallocModeHelper {
 public:
  static std::optional<FallocMode> from_bits(uint32_t mode) {
    // Translate mode values to corresponding FallocMode enum variants
    if (mode == 0) {
      return FallocMode::Allocate;
    } else if (mode == FALLOC_FL_KEEP_SIZE) {
      return FallocMode::Allocate;
    } else if (mode == (FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE)) {
      return FallocMode::PunchHole;
    } else if (mode == FALLOC_FL_COLLAPSE_RANGE) {
      return FallocMode::Collapse;
    } else if (mode == FALLOC_FL_ZERO_RANGE) {
      return FallocMode::Zero;
    } else if (mode == (FALLOC_FL_ZERO_RANGE | FALLOC_FL_KEEP_SIZE)) {
      return FallocMode::Zero;
    } else if (mode == FALLOC_FL_INSERT_RANGE) {
      return FallocMode::InsertRange;
    } else if (mode == FALLOC_FL_UNSHARE_RANGE) {
      return FallocMode::UnshareRange;
    } else {
      return std::nullopt;  // No matching FallocMode variant
    }
  }
};

class FileOps;
class FsNodeOps {
 public:
  // Delegate the access check to the node. Returns `Err(ENOSYS)` if the kernel must handle the
  /// access check by itself.
  virtual fit::result<Errno> check_access(const FsNode& node, const CurrentTask& current_task,
                                          int access) {
    return fit::error(errno(ENOSYS));
  }

  /// Build the `FileOps` for the file associated to this node.
  ///
  /// The returned FileOps will be used to create a FileObject, which might
  /// be assigned an FdNumber.
  virtual fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(
      /*FileOpsCore& locked,*/ const FsNode& node, const CurrentTask& current_task,
      OpenFlags flags) = 0;

  /// Find an existing child node and populate the child parameter. Return the node.
  ///
  /// The child parameter is an empty node. Operations other than initialize may panic before
  /// initialize is called.
  virtual fit::result<Errno, FsNodeHandle> lookup(const FsNode& node,
                                                  const CurrentTask& current_task,
                                                  const FsStr& name) {
    return fit::error(errno(ENOENT));
  }

  /// Create and return the given child node.
  ///
  /// The mode field of the FsNodeInfo indicates what kind of child to
  /// create.
  ///
  /// This function is never called with FileMode::IFDIR. The mkdir function
  /// is used to create directories instead.
  virtual fit::result<Errno, FsNodeHandle> mknod(/*FileOpsCore& locked,*/ const FsNode& node,
                                                 const CurrentTask& current_task, const FsStr& name,
                                                 FileMode mode, DeviceType dev, FsCred owner) {
    return fit::error(errno(EROFS));
  }

  /// Create and return the given child node as a subdirectory.
  virtual fit::result<Errno, FsNodeHandle> mkdir(const FsNode& node,
                                                 const CurrentTask& current_task, const FsStr& name,
                                                 FileMode mode, FsCred owner) {
    return fit::error(errno(EROFS));
  }

  /// Creates a symlink with the given `target` path.
  virtual fit::result<Errno, FsNodeHandle> create_symlink(const FsNode& node,
                                                          const CurrentTask& current_task,
                                                          const FsStr& name, const FsStr& target,
                                                          FsCred owner) {
    return fit::error(errno(EROFS));
  }

  /// Creates an anonymous file.
  ///
  /// The FileMode::IFMT of the FileMode is always FileMode::IFREG.
  ///
  /// Used by O_TMPFILE.
  virtual fit::result<Errno, FsNodeHandle> create_tmpfile(const FsNode& node,
                                                          const CurrentTask& current_task,
                                                          FileMode mode, FsCred owner) {
    return fit::error(errno(EOPNOTSUPP));
  }

  /// Reads the symlink from this node.
  virtual fit::result<Errno, SymlinkTarget> readlink(const FsNode& node,
                                                     const CurrentTask& current_task) {
    return fit::error(errno(EINVAL));
  }

  /// Create a hard link with the given name to the given child.
  virtual fit::result<Errno> link(const FsNode& node, const CurrentTask& current_task,
                                  const FsStr& name, const FsNodeHandle& child) {
    return fit::error(errno(EPERM));
  }

  /// Remove the child with the given name, if the child exists.
  ///
  /// The UnlinkKind parameter indicates whether the caller intends to unlink
  /// a directory or a non-directory child.
  virtual fit::result<Errno> unlink(const FsNode& node, const CurrentTask& current_task,
                                    const FsStr& name, const FsNodeHandle& child) = 0;

  /// Change the length of the file.
  virtual fit::result<Errno> truncate(const FsNode& node, const CurrentTask& current_task,
                                      uint64_t length) {
    return fit::error(errno(EINVAL));
  }

  /// Manipulate allocated disk space for the file.
  virtual fit::result<Errno> allocate(const FsNode& node, const CurrentTask& current_task,
                                      FallocMode mode, uint64_t offset, uint64_t length) {
    return fit::error(errno(EINVAL));
  }

  /// Update the supplied info with initial state (e.g. size) for the node.
  ///
  /// FsNode calls this method when created, to allow the FsNodeOps to
  /// set appropriate initial values in the FsNodeInfo.
  virtual void initial_info(FsNodeInfo& info) {}

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
  virtual fit::result<Errno, FsNodeInfo> refresh_info(const FsNode& node,
                                                      const CurrentTask& current_task,
                                                      RwLock<FsNodeInfo>& info) {
    return fit::ok(*info.Read());
  }

  /// Indicates if the filesystem can manage the timestamps (i.e. atime, ctime, and mtime).
  ///
  /// Starnix updates the timestamps in node.info directly. However, if the filesystem can manage
  /// the timestamps, then Starnix does not need to do so. `node.info`` will be refreshed with the
  /// timestamps from the filesystem by calling `refresh_info(..)`.
  virtual bool filesystem_manages_timestamps(const FsNode& node) { return false; }

  /// Update node attributes persistently.
  virtual fit::result<Errno> update_attributes(const FsNodeInfo& info, int has) {
    return fit::ok();
  }

  /// Get an extended attribute on the node.
  ///
  /// An implementation can systematically return a value. Otherwise, if `max_size` is 0, it can
  /// instead return the size of the attribute, and can return an ERANGE error if max_size is not
  /// 0, and lesser than the required size.
  virtual fit::result<Errno, ValueOrSize<FsString>> get_xattr(const FsNode& node,
                                                              const CurrentTask& current_task,
                                                              const FsStr& name, size_t max_size) {
    return fit::error(errno(ENOTSUP));
  }

  /// Set an extended attribute on the node.
  virtual fit::result<Errno> set_xattr(const FsNode& node, const CurrentTask& current_task,
                                       const FsStr& name, const FsStr& value, XattrOp op) {
    return fit::error(errno(ENOTSUP));
  }

  virtual fit::result<Errno> remove_xattr(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name) {
    return fit::error(errno(ENOTSUP));
  }

  /// An implementation can systematically return a value. Otherwise, if `max_size` is 0, it can
  /// instead return the size of the 0 separated string needed to represent the value, and can
  /// return an ERANGE error if max_size is not 0, and lesser than the required size.
  virtual fit::result<Errno, ValueOrSize<std::vector<FsString>>> list_xattrs(
      const FsNode& node, const CurrentTask& current_task, size_t max_size) {
    return fit::error(errno(ENOTSUP));
  }

  /// Called when the FsNode is freed by the Kernel.
  virtual fit::result<Errno> forget(const FsNode& node, const CurrentTask& current_task) {
    return fit::ok();
  }

  ////////////////////
  // FS-Verity operations

  /// Marks that FS-Verity is being built. Writes fsverity descriptor and merkle tree, the latter
  /// computed by the filesystem.
  /// This should ensure there are no writable file handles. Returns EEXIST if the file was
  /// already fsverity-enabled. Returns EBUSY if this ioctl was already running on this file.
  virtual fit::result<Errno> enable_fsverity(const fsverity_descriptor& descriptor) {
    return fit::error(errno(ENOTSUP));
  }

  /// Read fsverity descriptor, if the node is fsverity-enabled. Else returns ENODATA.
  virtual fit::result<Errno, fsverity_descriptor> get_fsverity_descriptor(uint8_t log_blocksize) {
    return fit::error(errno(ENOTSUP));
  }

  // C++
  virtual ~FsNodeOps() = default;
};

/// Implements [`FsNodeOps`] methods in a way that makes sense for symlinks.
/// You must implement [`FsNodeOps::readlink`].
#define fs_node_impl_symlink                                                                   \
  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(                                \
      /*FileOpsCore& locked,*/ const FsNode& node, const CurrentTask& current_task, int flags) \
      const final {                                                                            \
    panic("Symlink nodes cannot be opened.");                                                  \
  }                                                                                            \
  using __fs_node_impl_symlink_force_semicolon = int

#define fs_node_impl_dir_readonly                                                                  \
  fit::result<Errno, FsNodeHandle> mkdir(const FsNode& node, const CurrentTask& current_task,      \
                                         const FsStr& name, FileMode mode, FsCred owner) final {   \
    return fit::error(errno(EROFS));                                                               \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, FsNodeHandle> mknod(/*FileOpsCore& locked,*/ const FsNode& node,              \
                                         const CurrentTask& current_task, const FsStr& name,       \
                                         FileMode mode, DeviceType dev, FsCred owner) final {      \
    return fit::error(errno(EROFS));                                                               \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, FsNodeHandle> create_symlink(                                                 \
      const FsNode& node, const CurrentTask& current_task, const FsStr& name, const FsStr& target, \
      FsCred owner) {                                                                              \
    return fit::error(errno(EROFS));                                                               \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno> link(const FsNode& node, const CurrentTask& current_task, const FsStr& name,  \
                          const FsNodeHandle& child) final {                                       \
    return fit::error(errno(EROFS));                                                               \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno> unlink(const FsNode& node, const CurrentTask& current_task,                   \
                            const FsStr& name, const FsNodeHandle& child) final {                  \
    return fit::error(errno(EROFS));                                                               \
  }                                                                                                \
  using __fs_node_impl_dir_readonly_force_semicolon = int

/// Implements [`FsNodeOps::set_xattr`] by delegating to another [`FsNodeOps`]
/// object.
#define fs_node_impl_xattr_delegate(delegate)                                                  \
  fit::result<Errno, ValueOrSize<FsString>> get_xattr(                                         \
      const FsNode& node, const CurrentTask& current_task, const FsStr& name, size_t max_size) \
      final {                                                                                  \
    auto get_xattr_result = delegate.get_xattr(name);                                          \
    if (get_xattr_result.is_error())                                                           \
      return get_xattr_result.take_error();                                                    \
    return fit::ok(get_xattr_result.value());                                                  \
  }                                                                                            \
                                                                                               \
  fit::result<Errno> set_xattr(const FsNode& node, const CurrentTask& current_task,            \
                               const FsStr& name, const FsStr& value, XattrOp op) final {      \
    return delegate.set_xattr(name, value, op);                                                \
  }                                                                                            \
                                                                                               \
  fit::result<Errno> remove_xattr(const FsNode& node, const CurrentTask& current_task,         \
                                  const FsStr& name) final {                                   \
    return delegate.remove_xattr(name);                                                        \
  }                                                                                            \
                                                                                               \
  fit::result<Errno, ValueOrSize<std::vector<FsString>>> list_xattrs(                          \
      const FsNode& node, const CurrentTask& current_task, size_t max_size) final {            \
    return fit::error(errno(ENOTSUP));                                                         \
  }                                                                                            \
  using __fs_node_impl_xattr_delegate_force_semicolon = int

/// Stubs out [`FsNodeOps`] methods that only apply to directories.
#define fs_node_impl_not_dir                                                                       \
  fit::result<Errno, FsNodeHandle> lookup(const FsNode& node, const CurrentTask& current_task,     \
                                          const FsStr& name) final {                               \
    return fit::error(errno(ENOTDIR));                                                             \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, FsNodeHandle> mknod(/*FileOpsCore& locked,*/ const FsNode& node,              \
                                         const CurrentTask& current_task, const FsStr& name,       \
                                         FileMode mode, DeviceType dev, FsCred owner) final {      \
    return fit::error(errno(ENOTDIR));                                                             \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, FsNodeHandle> mkdir(const FsNode& node, const CurrentTask& current_task,      \
                                         const FsStr& name, FileMode mode, FsCred owner) final {   \
    return fit::error(errno(ENOTDIR));                                                             \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno, FsNodeHandle> create_symlink(                                                 \
      const FsNode& node, const CurrentTask& current_task, const FsStr& name, const FsStr& target, \
      FsCred owner) {                                                                              \
    return fit::error(errno(ENOTDIR));                                                             \
  }                                                                                                \
                                                                                                   \
  fit::result<Errno> unlink(const FsNode& node, const CurrentTask& current_task,                   \
                            const FsStr& name, const FsNodeHandle& child) final {                  \
    return fit::error(errno(ENOTDIR));                                                             \
  }                                                                                                \
  using __fs_node_impl_not_dir_force_semicolon = int

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_H_
