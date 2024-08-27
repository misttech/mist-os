// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/fs/tmpfs.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/module.h>
#include <lib/mistos/starnix/kernel/vfs/vmo_file.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/vfs.h>

#include <string_view>

#include <fbl/alloc_checker.h>
#include <ktl/unique_ptr.h>

#include <linux/magic.h>

namespace starnix {

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

  auto fs = FileSystem::New(kernel, {CacheModeType::Permanent}, std::move(tmpfs), options);
  fbl::HashTable<FsString, ktl::unique_ptr<fs_args::HashableFsString>> mount_options;
  fs_args::generic_parse_mount_options(fs->options().params, &mount_options);

  auto result = [&]() -> fit::result<Errno, FileMode> {
    auto mode_str = mount_options.erase("mode");
    if (mode_str) {
      return FileMode::from_string({mode_str->value.data(), mode_str->value.size()});
    } else {
      return fit::ok(FILE_MODE(IFDIR, 0777));
    }
  }();
  if (result.is_error()) {
    return result.take_error();
  }
  FileMode mode = result.value();

  auto result_uid = [&]() -> fit::result<Errno, uid_t> {
    auto uid_str = mount_options.erase("uid");
    if (uid_str) {
      return fs_args::parse<uid_t>({uid_str->value.data(), uid_str->value.size()});
    } else {
      return fit::ok(0);
    }
  }();
  if (result_uid.is_error()) {
    return result.take_error();
  }
  uid_t uid = result_uid.value();

  auto result_gid = [&]() -> fit::result<Errno, gid_t> {
    auto gid_str = mount_options.erase("gid");
    if (gid_str) {
      return fs_args::parse<uid_t>({gid_str->value.data(), gid_str->value.size()});
    } else {
      return fit::ok(0);
    }
  }();
  if (result_gid.is_error()) {
    return result.take_error();
  }
  uid_t gid = result_gid.value();

  auto root_node = FsNode::new_root_with_properties(TmpfsDirectory::New(),
                                                    [&mode, &uid, &gid](FsNodeInfo& info) -> void {
                                                      info.chmod(mode);
                                                      info.uid = uid;
                                                      info.gid = gid;
                                                    });
  fs->set_root_node(root_node);

  /*
  if !mount_options.is_empty() {
      track_stub!(
          TODO("https://fxbug.dev/322873419"),
          "unknown tmpfs options, see logs for strings"
      );
      log_warn!(
          "Unknown tmpfs options: {}",
          itertools::join(mount_options.iter().map(|(k, v)| format!("{k}={v}")), ",")
      );
  }
*/

  return fit::ok(std::move(fs));
}

fit::result<Errno, struct statfs> TmpFs::statfs(const FileSystem& fs,
                                                const CurrentTask& current_task) {
  struct statfs stat = default_statfs(TMPFS_MAGIC);
  // Pretend we have a ton of free space.
  stat.f_blocks = 0x100000000;
  stat.f_bavail = 0x100000000;
  stat.f_bfree = 0x100000000;
  return fit::ok(stat);
}

TmpfsDirectory* TmpfsDirectory::New() {
  fbl::AllocChecker ac;
  auto dir = new (&ac) TmpfsDirectory();
  ZX_ASSERT(ac.check());
  return dir;
}

fit::result<Errno, ktl::unique_ptr<FileOps>> TmpfsDirectory::create_file_ops(
    /*FileOpsCore& locked,*/ const FsNode& node, const CurrentTask& current_task, OpenFlags flags) {
  return fit::error(errno(ENOTSUP));
}

fit::result<Errno, FsNodeHandle> TmpfsDirectory::mkdir(const FsNode& node,
                                                       const CurrentTask& current_task,
                                                       const FsStr& name, FileMode mode,
                                                       FsCred owner) {
  node.update_info<void>([](FsNodeInfo& info) { info.link_count += 1; });

  {
    auto data = child_count_.Lock();
    data = *data + 1;
  }

  return fit::ok(node.fs()->create_node(current_task,
                                        ktl::unique_ptr<FsNodeOps>(TmpfsDirectory::New()),
                                        FsNodeInfo::new_factory(mode, owner)));
}

fit::result<Errno, FsNodeHandle> TmpfsDirectory::mknod(const FsNode& node,
                                                       const CurrentTask& current_task,
                                                       const FsStr& name, FileMode mode,
                                                       DeviceType dev, FsCred owner) {
  auto child_result = create_child_node(current_task, node, mode, dev, owner);
  if (child_result.is_error())
    return child_result.take_error();

  {
    auto data = child_count_.Lock();
    data = *data + 1;
  }

  return fit::ok(child_result.value());
}

fit::result<Errno, FsNodeHandle> TmpfsDirectory::create_tmpfile(const FsNode& node,
                                                                const CurrentTask& current_task,
                                                                FileMode mode, FsCred owner) {
  return fit::error(errno(ENOTSUP));
}

fit::result<Errno> TmpfsDirectory::link(const FsNode& node, const CurrentTask& current_task,
                                        const FsStr& name, const FsNodeHandle& child) {
  return fit::error(errno(ENOTSUP));
}

fit::result<Errno> TmpfsDirectory::unlink(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name, const FsNodeHandle& child) {
  return fit::error(errno(ENOTSUP));
}

fit::result<Errno, FsNodeHandle> create_child_node(const CurrentTask& current_task,
                                                   const FsNode& parent, FileMode mode,
                                                   DeviceType dev, FsCred owner) {
  ktl::unique_ptr<FsNodeOps> ops;
  auto fmt = mode.fmt();
  if (fmt == FileMode::IFREG) {
    auto new_result = VmoFileNode::New();
    if (new_result.is_error())
      return new_result.take_error();
    ops = ktl::unique_ptr<FsNodeOps>(new_result.value());
  } else if (fmt == FileMode::IFIFO || fmt == FileMode::IFBLK || fmt == FileMode::IFCHR ||
             fmt == FileMode::IFSOCK) {
    ops = ktl::unique_ptr<FsNodeOps>(TmpfsSpecialNode::New());
  } else {
    return fit::error(errno(EACCES));
  }

  auto child = parent.fs()->create_node(current_task, std::move(ops),
                                        [mode, owner, dev](ino_t id) -> FsNodeInfo {
                                          auto info = FsNodeInfo::New(id, mode, owner);
                                          info.rdev = dev;
                                          // blksize is PAGE_SIZE for in memory node.
                                          info.blksize = PAGE_SIZE;
                                          return info;
                                        });

  if (fmt == FileMode::IFREG) {
    // For files created in tmpfs, forbid sealing, by sealing the seal operation.
    /* child.write_guard_state.lock().enable_sealing(SealFlags::SEAL); */
  }
  return fit::ok(child);
}

TmpfsSpecialNode* TmpfsSpecialNode::New() {
  fbl::AllocChecker ac;
  auto node = new (&ac) TmpfsSpecialNode();
  ZX_ASSERT(ac.check());
  return node;
}

}  // namespace starnix
