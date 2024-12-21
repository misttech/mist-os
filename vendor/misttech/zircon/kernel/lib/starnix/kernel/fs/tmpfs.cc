// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/fs/tmpfs.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/directory_file.h>
#include <lib/mistos/starnix/kernel/vfs/dirent_sink.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/memory_file.h>
#include <lib/mistos/starnix/kernel/vfs/module.h>
#include <lib/mistos/starnix/kernel/vfs/symlink_node.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/vfs.h>
#include <trace.h>

#include <fbl/alloc_checker.h>
#include <ktl/string_view.h>
#include <ktl/unique_ptr.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#include <linux/magic.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {
namespace {

class TmpfsSpecialNode : public FsNodeOps {
 private:
  MemoryXattrStorage xattrs_;

 public:
  /// impl TmpfsSpecialNode
  static TmpfsSpecialNode* New() {
    fbl::AllocChecker ac;
    auto ptr = new (&ac) TmpfsSpecialNode();
    ZX_ASSERT(ac.check());
    return ptr;
  }

  /// impl FsNodeOps
  fs_node_impl_not_dir();
  fs_node_impl_xattr_delegate(xattrs_);

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(const FsNode& node,
                                                               const CurrentTask& current_task,
                                                               OpenFlags flags) const final {
    PANIC("Special nodes cannot be opened.\n");
  }

 private:
  TmpfsSpecialNode() : xattrs_(MemoryXattrStorage::Default()) {}
};

}  // namespace

FileSystemHandle TmpFs::new_fs(const fbl::RefPtr<Kernel>& kernel) {
  if (auto result = TmpFs::new_fs_with_options(kernel, {}); result.is_error()) {
    ZX_PANIC("empty options cannot fail");
  } else {
    return result.value();
  }
}

fit::result<Errno, FileSystemHandle> TmpFs::new_fs_with_options(const fbl::RefPtr<Kernel>& kernel,
                                                                FileSystemOptions options) {
  fbl::AllocChecker ac;
  auto tmpfs = new (&ac) TmpFs();
  if (!ac.check()) {
    return fit::error(errno(ENOMEM));
  }

  auto fs = FileSystem::New(kernel, {.type = CacheMode::Type::Permanent}, tmpfs, ktl::move(options))
      _EP(fs);
  auto mount_options = fs->options_.params;

  auto result = [&]() -> fit::result<Errno, FileMode> {
    auto mode_str = mount_options.remove("mode");
    if (mode_str) {
      return FileMode::from_string({mode_str->data(), mode_str->size()});
    }
    return fit::ok(FILE_MODE(IFDIR, 0777));
  }() _EP(result);

  FileMode mode = result.value();

  auto result_uid = [&]() -> fit::result<Errno, uid_t> {
    auto uid_str = mount_options.remove("uid");
    if (uid_str) {
      return parse<uid_t>({uid_str->data(), uid_str->size()});
    }
    return fit::ok(0);
  }() _EP(result_uid);

  uid_t uid = result_uid.value();
  auto result_gid = [&]() -> fit::result<Errno, gid_t> {
    auto gid_str = mount_options.remove("gid");
    if (gid_str) {
      return parse<uid_t>({gid_str->data(), gid_str->size()});
    }
    return fit::ok(0);
  }() _EP(result_gid);
  uid_t gid = result_gid.value();

  auto root_node = FsNode::new_root_with_properties(TmpfsDirectory::New(),
                                                    [&mode, &uid, &gid](FsNodeInfo& info) -> void {
                                                      info.chmod(mode);
                                                      info.uid_ = uid;
                                                      info.gid_ = gid;
                                                    });
  fs->set_root_node(root_node);

  if (!mount_options.is_empty()) {
    /*track_stub!(
        TODO("https://fxbug.dev/322873419"),
        "unknown tmpfs options, see logs for strings"
    );*/
    /*log_warn!(
        "Unknown tmpfs options: {}",
        itertools::join(mount_options.iter().map(|(k, v)| format!("{k}={v}")), ",")
    );*/
  }

  return fit::ok(ktl::move(fs.value()));
}

fit::result<Errno, struct statfs> TmpFs::statfs(const FileSystem& fs,
                                                const CurrentTask& current_task) const {
  struct statfs stat = default_statfs(TMPFS_MAGIC);
  // Pretend we have a ton of free space.
  stat.f_blocks = 0x100000000;
  stat.f_bavail = 0x100000000;
  stat.f_bfree = 0x100000000;
  return fit::ok(stat);
}

const FsStr& TmpFs::name() const { return name_; }

fit::result<Errno> TmpFs::rename(const FileSystem& fs, const CurrentTask& current_task,
                                 const FsNodeHandle& old_parent, const FsStr& old_name,
                                 const FsNodeHandle& new_parent, const FsStr& new_name,
                                 const FsNodeHandle& renamed,
                                 ktl::optional<FsNodeHandle> replaced) const {
  auto child_count = [](const FsNodeHandle& node) -> starnix_sync::MutexGuard<uint32_t> {
    // The following casts are safe, unless something is seriously wrong:
    // - The filesystem should not be asked to rename nodes that it doesn't handle
    // - Parents in a rename operation need to be directories
    // - TmpfsDirectory is the ops for directories in this filesystem
    auto downcast = node->downcast_ops<TmpfsDirectory>();
    ZX_ASSERT(downcast.has_value());
    return downcast.value()->child_count_.Lock();
  };

  if (replaced.has_value()) {
    if ((*replaced)->is_dir()) {
      // Ensure replaced is empty
      if (*child_count(*replaced) != 0) {
        return fit::error(errno(ENOTEMPTY));
      }
    }
  }

  *child_count(old_parent) -= 1;
  *child_count(new_parent) += 1;
  if (renamed->is_dir()) {
    old_parent->update_info<void>([](FsNodeInfo& info) { info.link_count_ -= 1; });
    new_parent->update_info<void>([](FsNodeInfo& info) { info.link_count_ += 1; });
  }

  // Fix the wrong changes to new_parent due to the fact that the target element has
  // been replaced instead of added
  if (replaced.has_value()) {
    if ((*replaced)->is_dir()) {
      new_parent->update_info<void>([](FsNodeInfo& info) { info.link_count_ -= 1; });
    }
    *child_count(new_parent) -= 1;
  }

  return fit::ok();
}

fit::result<Errno> TmpFs::exchange(const FileSystem& fs, const CurrentTask& current_task,
                                   const FsNodeHandle& node1, const FsNodeHandle& parent1,
                                   const FsStr& name1, const FsNodeHandle& node2,
                                   const FsNodeHandle& parent2, const FsStr& name2) const {
  if (node1->is_dir()) {
    parent1->update_info<void>([](FsNodeInfo& info) { info.link_count_ -= 1; });
    parent2->update_info<void>([](FsNodeInfo& info) { info.link_count_ += 1; });
  }

  if (node2->is_dir()) {
    parent1->update_info<void>([](FsNodeInfo& info) { info.link_count_ += 1; });
    parent2->update_info<void>([](FsNodeInfo& info) { info.link_count_ -= 1; });
  }

  return fit::ok();
}

TmpFs::~TmpFs() { LTRACE_ENTRY_OBJ; }

TmpfsDirectory::TmpfsDirectory() : xattrs_(MemoryXattrStorage::Default()) {}

TmpfsDirectory* TmpfsDirectory::New() {
  fbl::AllocChecker ac;
  auto dir = new (&ac) TmpfsDirectory();
  ZX_ASSERT(ac.check());
  return dir;
}

fit::result<Errno, ktl::unique_ptr<FileOps>> TmpfsDirectory::create_file_ops(
    const FsNode& node, const CurrentTask& current_task, OpenFlags flags) const {
  return fit::ok(ktl::unique_ptr<FileOps>(MemoryDirectoryFile::New()));
}

fit::result<Errno, FsNodeHandle> TmpfsDirectory::mkdir(const FsNode& node,
                                                       const CurrentTask& current_task,
                                                       const FsStr& name, FileMode mode,
                                                       FsCred owner) const {
  node.update_info<void>([](FsNodeInfo& info) { info.link_count_ += 1; });
  *child_count_.Lock() += 1;
  return fit::ok(node.fs()->create_node(current_task,
                                        ktl::unique_ptr<FsNodeOps>(TmpfsDirectory::New()),
                                        FsNodeInfo::new_factory(mode, owner)));
}

fit::result<Errno, FsNodeHandle> TmpfsDirectory::mknod(const FsNode& node,
                                                       const CurrentTask& current_task,
                                                       const FsStr& name, FileMode mode,
                                                       DeviceType dev, FsCred owner) const {
  auto child_result = create_child_node(current_task, node, mode, dev, owner) _EP(child_result);
  *child_count_.Lock() += 1;
  return fit::ok(child_result.value());
}

fit::result<Errno, FsNodeHandle> TmpfsDirectory::create_symlink(const FsNode& node,
                                                                const CurrentTask& current_task,
                                                                const FsStr& name,
                                                                const FsStr& target,
                                                                FsCred owner) const {
  *child_count_.Lock() += 1;
  auto [link, info] = SymlinkNode::New(target, owner);
  return fit::ok(node.fs()->create_node(current_task, ktl::move(link), info));
}

fit::result<Errno, FsNodeHandle> TmpfsDirectory::create_tmpfile(const FsNode& node,
                                                                const CurrentTask& current_task,
                                                                FileMode mode, FsCred owner) const {
  ZX_ASSERT(mode.is_reg());
  return create_child_node(current_task, node, mode, DeviceType::NONE, owner);
}

fit::result<Errno> TmpfsDirectory::link(const FsNode& node, const CurrentTask& current_task,
                                        const FsStr& name, const FsNodeHandle& child) const {
  child->update_info<void>([](FsNodeInfo& info) { info.link_count_ += 1; });
  *child_count_.Lock() += 1;
  return fit::ok();
}

fit::result<Errno> TmpfsDirectory::unlink(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name, const FsNodeHandle& child) const {
  if (child->is_dir()) {
    node.update_info<void>([](FsNodeInfo& info) { info.link_count_ -= 1; });
  }
  child->update_info<void>([](FsNodeInfo& info) { info.link_count_ -= 1; });
  *child_count_.Lock() -= 1;
  return fit::ok();
}

fit::result<Errno, FsNodeHandle> create_child_node(const CurrentTask& current_task,
                                                   const FsNode& parent, FileMode mode,
                                                   DeviceType dev, FsCred owner) {
  ktl::unique_ptr<FsNodeOps> ops;
  auto fmt = mode.fmt();
  if (fmt == FileMode::IFREG) {
    auto new_result = MemoryFileNode::New() _EP(new_result);
    ops = ktl::unique_ptr<FsNodeOps>(new_result.value());
  } else if (fmt == FileMode::IFIFO || fmt == FileMode::IFBLK || fmt == FileMode::IFCHR ||
             fmt == FileMode::IFSOCK) {
    ops = ktl::unique_ptr<FsNodeOps>(TmpfsSpecialNode::New());
  } else {
    return fit::error(errno(EACCES));
  }

  auto child = parent.fs()->create_node(current_task, ktl::move(ops),
                                        [mode, owner, dev](ino_t id) -> FsNodeInfo {
                                          auto info = FsNodeInfo::New(id, mode, owner);
                                          info.rdev_ = dev;
                                          // blksize is PAGE_SIZE for in memory node.
                                          info.blksize_ = PAGE_SIZE;
                                          return info;
                                        });

  if (fmt == FileMode::IFREG) {
    // For files created in tmpfs, forbid sealing, by sealing the seal operation.
    /* child.write_guard_state.lock().enable_sealing(SealFlags::SEAL); */
  }
  return fit::ok(child);
}

fit::result<Errno, FileSystemHandle> tmp_fs(const CurrentTask& current_task,
                                            FileSystemOptions options) {
  return TmpFs::new_fs_with_options(current_task->kernel(), options);
}

}  // namespace starnix
