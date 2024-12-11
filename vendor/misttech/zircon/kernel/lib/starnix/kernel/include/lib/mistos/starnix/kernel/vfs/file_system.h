// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_SYSTEM_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_SYSTEM_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/memory/weak_ptr.h>
#include <lib/mistos/starnix/kernel/vfs/fs_args.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_info.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/util/onecell.h>
#include <lib/starnix_sync/locks.h>
#include <zircon/compiler.h>

#include <fbl/intrusive_hash_table.h>
#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/unique_ptr.h>
#include <ktl/variant.h>

namespace starnix {

const size_t DEFAULT_LRU_CAPACITY = 32;

struct Dummy : public fbl::SinglyLinkedListable<Dummy*> {
  static size_t GetHash(uint64_t key) { return static_cast<size_t>(key); }
};

struct LruCache {
  explicit LruCache(size_t c) : capacity(c) {}

  size_t capacity;

  mutable starnix_sync::Mutex<fbl::HashTable<fbl::RefPtr<DirEntry>, Dummy*>> entries;
};

struct Permanent {
  mutable starnix_sync::Mutex<fbl::HashTable<fbl::RefPtr<DirEntry>, Dummy*>> entries;
};

using Entries = ktl::variant<ktl::monostate, ktl::unique_ptr<Permanent>, ktl::unique_ptr<LruCache>>;

// Configuration for CacheMode::Cached.
struct CacheConfig {
  size_t capacity = DEFAULT_LRU_CAPACITY;
};

enum class CacheModeType : uint8_t {
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
  /// Filesystem options passed as the last argument to mount().
  MountParams params;
};

class FileSystemOps;
class Kernel;
class FsNodeOps;
class FsNode;
using WeakFsNodeHandle = mtl::WeakPtr<FsNode>;

/// A file system that can be mounted in a namespace.
class FileSystem : public fbl::RefCountedUpgradeable<FileSystem> {
 public:
  mtl::WeakPtr<Kernel> kernel_;

 private:
  OnceCell<DirEntryHandle> root_;

  mutable ktl::atomic<uint64_t> next_node_id_;

  ktl::unique_ptr<FileSystemOps> ops_;

 public:
  /// The options specified when mounting the filesystem. Saved here for display in
  /// /proc/[pid]/mountinfo.
  FileSystemOptions options_;

  /// The device ID of this filesystem. Returned in the st_dev field when stating an inode in
  /// this filesystem.
  DeviceType dev_id_;

  /// A file-system global mutex to serialize rename operations.
  ///
  /// This mutex is useful because the invariants enforced during a rename
  /// operation involve many DirEntry objects. In the future, we might be
  /// able to remove this mutex, but we will need to think carefully about
  /// how rename operations can interleave.
  ///
  /// See DirEntry::rename.
  // pub rename_mutex: Mutex<()>,

 private:
  /// The FsNode cache for this file system.
  ///
  /// When two directory entries are hard links to the same underlying inode,
  /// this cache lets us re-use the same FsNode object for both directory
  /// entries.
  ///
  /// Rather than calling FsNode::new directly, file systems should call
  /// FileSystem::get_or_create_node to see if the FsNode already exists in
  /// the cache.
  mutable starnix_sync::Mutex<fbl::HashTable<ino_t, WeakFsNodeHandle>> nodes_;

  /// DirEntryHandle cache for the filesystem. Holds strong references to DirEntry objects. For
  /// filesystems with permanent entries, this will hold a strong reference to every node to make
  /// sure it doesn't get freed without being explicitly unlinked. Otherwise, entries are
  /// maintained in an LRU cache.
  Entries entries_;

  /// Hack meant to stand in for the fs_use_trans selinux feature. If set, this value will be set
  /// as the selinux label on any newly created inodes in the filesystem.
  // pub selinux_context: OnceCell<FsString>,

  // impl FileSystem
 public:
  /// Create a new filesystem.
  static FileSystemHandle New(const fbl::RefPtr<Kernel>& kernel, CacheMode cache_mode,
                              FileSystemOps* ops, FileSystemOptions options);

  void set_root(FsNodeOps* root);

  // Set up the root of the filesystem. Must not be called more than once.
  void set_root_node(FsNode* root);

  /// Inserts a node in the FsNode cache.
  DirEntryHandle insert_node(FsNode* node);

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

  template <typename CreateFn>
  fit::result<Errno, FsNodeHandle> get_or_create_node(const CurrentTask& current_task,
                                                      ktl::optional<ino_t> node_id,
                                                      CreateFn&& create_fn) {
    static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateFn, ino_t>);
    return get_and_validate_or_create_node(
        current_task, node_id, [](const FsNodeHandle&) -> bool { return true; }, create_fn);
  }

  /// Get a node that is validated with the callback, or create an FsNode for
  /// this file system.
  ///
  /// If node_id is Some, then this function checks the node cache to
  /// determine whether this node is already open. If so, the function
  /// returns the existing FsNode if it passes the validation check. If no
  /// node exists, or a node does but fails the validation check, the function
  /// calls the given create_fn function to create the FsNode.
  ///
  /// If node_id is None, then this function assigns a new identifier number
  /// and calls the given create_fn function to create the FsNode with the
  /// assigned number.
  ///
  /// Returns Err only if create_fn returns Err.
  template <typename ValidateFn, typename CreateFn>
  fit::result<Errno, FsNodeHandle> get_and_validate_or_create_node(const CurrentTask& current_task,
                                                                   ktl::optional<ino_t> node_id,
                                                                   ValidateFn&& validate_fn,
                                                                   CreateFn&& create_fn) {
    static_assert(std::is_invocable_r_v<bool, ValidateFn, const FsNodeHandle&>);
    static_assert(std::is_invocable_r_v<fit::result<Errno, FsNodeHandle>, CreateFn, ino_t>);
    return fit::ok(FsNodeHandle());
  }

  // File systems that produce their own IDs for nodes should invoke this
  // function. The ones who leave to this object to assign the IDs should
  // call |create_node|.
  FsNodeHandle create_node_with_id(const CurrentTask& current_task, ktl::unique_ptr<FsNodeOps> ops,
                                   ino_t id, FsNodeInfo info);

  // Used by BootFS (no current task available)
  FsNodeHandle create_node_with_id_and_creds(ktl::unique_ptr<FsNodeOps> ops, ino_t id,
                                             FsNodeInfo info,
                                             const starnix_uapi::Credentials& credentials);

  template <typename NodeInfoFn>
    requires std::is_invocable_r_v<FsNodeInfo, NodeInfoFn, ino_t>
  FsNodeHandle create_node(const CurrentTask& current_task, ktl::unique_ptr<FsNodeOps> ops,
                           NodeInfoFn&& info) {
    auto node_id = next_node_id();
    return create_node_with_id(current_task, ktl::move(ops), node_id, info(node_id));
  }

  /// Remove the given FsNode from the node cache.
  ///
  /// Called from the Release trait of FsNode.
  void remove_node(const FsNode& node);

  ino_t next_node_id() const;

  void did_create_dir_entry(const DirEntryHandle& entry);

  void will_destroy_dir_entry(const DirEntryHandle& entry);

  /// Informs the cache that the entry was used.
  void did_access_dir_entry(const DirEntryHandle& entry);

  /// Purges old entries from the cache. This is done as a separate step to avoid potential
  /// deadlocks that could occur if done at admission time (where locks might be held that are
  /// required when dropping old entries). This should be called after any new entries are
  /// admitted with no locks held that might be required for dropping entries.
  void purge_old_entries();

  FsStr name() const;

  // C++
  ~FileSystem();

 private:
  FileSystem(const fbl::RefPtr<Kernel>& kernel, ktl::unique_ptr<FileSystemOps> ops,
             FileSystemOptions options, Entries entries);

 public:
  mtl::WeakPtrFactory<FileSystem> weak_factory_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FILE_SYSTEM_H_
