// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_SYSTEM_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_SYSTEM_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/vfs/forward.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/util/onecell.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <zircon/compiler.h>

#include <atomic>
#include <functional>
#include <optional>

#include <fbl/intrusive_hash_table.h>
#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/unique_ptr.h>
#include <ktl/variant.h>

#include <asm-generic/statfs.h>
#include <linux/stat.h>

namespace starnix {

const size_t DEFAULT_LRU_CAPACITY = 32;

struct Dummy : public fbl::SinglyLinkedListable<Dummy*> {
  static size_t GetHash(uint64_t key) { return static_cast<size_t>(key); }
};

struct LruCache {
  LruCache(size_t c) : capacity(c) {}

  size_t capacity;

  DECLARE_MUTEX(LruCache) lock;
  fbl::HashTable<fbl::RefPtr<DirEntry>, Dummy*> entries __TA_GUARDED(lock);
};

struct Permanent {
  DECLARE_MUTEX(Permanent) lock;
  fbl::HashTable<fbl::RefPtr<DirEntry>, Dummy*> entries __TA_GUARDED(lock);
};

using Entries = ktl::variant<ktl::monostate, ktl::unique_ptr<Permanent>, ktl::unique_ptr<LruCache>>;

// Configuration for CacheMode::Cached.
struct CacheConfig {
  size_t capacity = DEFAULT_LRU_CAPACITY;
};

enum class CacheModeType {
  /// Entries are pemanent, instead of a cache of the backing storage. An example is tmpfs: the
  /// DirEntry tree *is* the backing storage, as opposed to ext4, which uses the DirEntry tree as
  /// a cache and removes unused nodes from it.
  Permanent,
  /// Entries are cached.
  Cached,
  /// Entries are uncached. This can be appropriate in cases where it is difficult for the
  /// filesystem to keep the cache coherent: e.g. the /proc/<pid>/task directory.
  Uncached
};

struct CacheMode {
  CacheModeType type;
  CacheConfig config;
};

struct FileSystemOptions {
  // The source string passed as the first argument to mount(), e.g. a block device.
  FsString source;
  // Flags kept per-superblock, i.e. included in MountFlags::STORED_ON_FILESYSTEM.
  MountFlags flags = MountFlags::empty();
  // Filesystem options passed as the last argument to mount().
  FsString params;
};

class FileSystemOps;

/// A file system that can be mounted in a namespace.
class FileSystem : private fbl::RefCountedUpgradeable<FileSystem> {
 public:
  using fbl::RefCountedUpgradeable<FileSystem>::AddRef;
  using fbl::RefCountedUpgradeable<FileSystem>::Release;
  using fbl::RefCountedUpgradeable<FileSystem>::Adopt;
  using fbl::RefCountedUpgradeable<FileSystem>::AddRefMaybeInDestructor;

  ~FileSystem();

  static FileSystemHandle New(const fbl::RefPtr<Kernel>& kernel, CacheMode cache_mode,
                              FileSystemOps* ops, FileSystemOptions options);

  void set_root(FsNodeOps* root);

  // Set up the root of the filesystem. Must not be called more than once.
  void set_root_node(FsNode* root);

  bool has_permanent_entries() const;

  // The root directory entry of this file system.
  //
  // Panics if this file system does not have a root directory.
  DirEntryHandle root();

  // Prepare a node for insertion in the node cache.
  //
  // Currently, apply the required selinux context if the selinux workaround is enabled on this
  // filesystem.
  WeakFsNodeHandle prepare_node_for_insertion(const CurrentTask& current_task,
                                              const FsNodeHandle& node);

  /// Get or create an FsNode for this file system.
  ///
  /// If node_id is Some, then this function checks the node cache to
  /// determine whether this node is already open. If so, the function
  /// returns the existing FsNode. If not, the function calls the given
  /// create_fn function to create the FsNode.
  ///
  /// If node_id is None, then this function assigns a new identifier number
  /// and calls the given create_fn function to create the FsNode with the
  /// assigned number.
  ///
  /// Returns Err only if create_fn returns Err.
  fit::result<Errno, FsNodeHandle> get_or_create_node(
      const CurrentTask& current_task, ktl::optional<ino_t> node_id,
      std::function<fit::result<Errno, FsNodeHandle>(ino_t)> create_fn);

  // File systems that produce their own IDs for nodes should invoke this
  // function. The ones who leave to this object to assign the IDs should
  // call |create_node|.
  FsNodeHandle create_node_with_id(const CurrentTask& current_task, ktl::unique_ptr<FsNodeOps> ops,
                                   ino_t id, FsNodeInfo info);

  FsNodeHandle create_node(const CurrentTask& current_task, ktl::unique_ptr<FsNodeOps> ops,
                           std::function<FsNodeInfo(ino_t)> info);

  /// Remove the given FsNode from the node cache.
  ///
  /// Called from the Release trait of FsNode.
  void remove_node(const FsNode& node);

  ino_t next_node_id();

  void did_create_dir_entry(const DirEntryHandle& entry);

  void will_destroy_dir_entry(const DirEntryHandle& entry);

  /// Informs the cache that the entry was used.
  void did_access_dir_entry(const DirEntryHandle& entry);

  /// Purges old entries from the cache. This is done as a separate step to avoid potential
  /// deadlocks that could occur if done at admission time (where locks might be held that are
  /// required when dropping old entries). This should be called after any new entries are
  /// admitted with no locks held that might be required for dropping entries.
  void purge_old_entries();

  const FileSystemOptions& options() const { return options_; }

  util::WeakPtr<Kernel> kernel() { return kernel_; }

 private:
  FileSystem(const fbl::RefPtr<Kernel>& kernel, ktl::unique_ptr<FileSystemOps> ops,
             FileSystemOptions options, Entries entries);

  util::WeakPtr<Kernel> kernel_;

  OnceCell<DirEntryHandle> root_;

  ktl::atomic<uint64_t> next_node_id_;

  ktl::unique_ptr<FileSystemOps> ops_;

  /// The options specified when mounting the filesystem. Saved here for display in
  /// /proc/[pid]/mountinfo.
  FileSystemOptions options_;

  /// The device ID of this filesystem. Returned in the st_dev field when stating an inode in
  /// this filesystem.
  // DeviceType dev_id_;

  /// A file-system global mutex to serialize rename operations.
  ///
  /// This mutex is useful because the invariants enforced during a rename
  /// operation involve many DirEntry objects. In the future, we might be
  /// able to remove this mutex, but we will need to think carefully about
  /// how rename operations can interleave.
  ///
  /// See DirEntry::rename.
  // pub rename_mutex: Mutex<()>,

  /// The FsNode cache for this file system.
  ///
  /// When two directory entries are hard links to the same underlying inode,
  /// this cache lets us re-use the same FsNode object for both directory
  /// entries.
  ///
  /// Rather than calling FsNode::new directly, file systems should call
  /// FileSystem::get_or_create_node to see if the FsNode already exists in
  /// the cache.
  // nodes: Mutex<HashMap<ino_t, WeakFsNodeHandle>>,
  DECLARE_MUTEX(FileSystem) nodes_lock_;
  fbl::HashTable<ino_t, WeakFsNodeHandle> nodes_ __TA_GUARDED(nodes_lock_);

  /// DirEntryHandle cache for the filesystem. Holds strong references to DirEntry objects. For
  /// filesystems with permanent entries, this will hold a strong reference to every node to make
  /// sure it doesn't get freed without being explicitly unlinked. Otherwise, entries are
  /// maintained in an LRU cache.
  Entries entries_;

  /// Hack meant to stand in for the fs_use_trans selinux feature. If set, this value will be set
  /// as the selinux label on any newly created inodes in the filesystem.
  // pub selinux_context: OnceCell<FsString>,
};

class FileSystemOps {
 public:
  virtual ~FileSystemOps() = default;

  /// Return information about this filesystem.
  ///
  /// A typical implementation looks like this:
  /// ```
  /// Ok(statfs::default(FILE_SYSTEM_MAGIC))
  /// ```
  /// or, if the filesystem wants to customize fields:
  /// ```
  /// Ok(statfs {
  ///     f_blocks: self.blocks,
  ///     ..statfs::default(FILE_SYSTEM_MAGIC)
  /// })
  /// ```
  virtual fit::result<Errno, struct statfs> statfs(const FileSystem& fs,
                                                   const CurrentTask& current_task) = 0;

  virtual const FsStr& name() = 0;

  /// Whether this file system generates its own node IDs.
  virtual bool generate_node_ids() { return false; }

  virtual fit::result<Errno> rename(const FileSystem& fs, const CurrentTask& current_task,
                                    const FsNodeHandle& old_parent, const FsStr& old_name,
                                    const FsNodeHandle& new_parent, const FsStr& new_name,
                                    const FsNodeHandle& renamed,
                                    const FsNodeHandle* replaced = nullptr) {
    return fit::error(errno(EROFS));
  }

  virtual fit::result<Errno> exchange(const FileSystem& fs, const CurrentTask& current_task,
                                      const FsNodeHandle& node1, const FsNodeHandle& parent1,
                                      const FsStr& name1, const FsNodeHandle& node2,
                                      const FsNodeHandle& parent2, const FsStr& name2) {
    return fit::error(errno(EINVAL));
  }

  /// Called when the filesystem is unmounted.
  virtual void unmount() {}
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_SYSTEM_H_
