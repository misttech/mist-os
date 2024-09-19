// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_STATIC_DIRECTORY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_STATIC_DIRECTORY_H_

#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/util/allocator.h>

#include <map>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>

namespace starnix {

using FileMode = starnix_uapi::FileMode;
using FsCred = starnix_uapi::FsCred;

class FileSystem;
class CurrentTask;
class FsNode;
using FsNodeHandle = fbl::RefPtr<FsNode>;
using BTreeMap = std::map<FsStr, FsNodeHandle, std::less<const FsStr>,
                          util::Allocator<ktl::pair<const FsStr, FsNodeHandle>>>;

class StaticDirectory : public fbl::RefCounted<StaticDirectory>, public FileOps, public FsNodeOps {
 private:
  BTreeMap entries_;

 public:
  StaticDirectory(BTreeMap entries) : entries_(ktl::move(entries)) {}

 public:
  /// impl FsNodeOps
  fs_node_impl_dir_readonly();

  fit::result<Errno, ktl::unique_ptr<FileOps>> create_file_ops(
      /*FileOpsCore& locked,*/ const FsNode& node, const CurrentTask& current_task,
      OpenFlags flags) final;

  fit::result<Errno, FsNodeHandle> lookup(const FsNode& node, const CurrentTask& current_task,
                                          const FsStr& name) final;

  /// impl FileOps
  fileops_impl_directory();
  // fileops_impl_noop_sync();

  fit::result<Errno, off_t> seek(const FileObject& file, const CurrentTask& current_task,
                                 off_t current_offset, SeekTarget target) final;

  fit::result<Errno> readdir(const FileObject& file, const CurrentTask& current_task,
                             DirentSink& sink) final;
};

class StaticDirectoryBuilder {
 private:
  fbl::RefPtr<FileSystem> fs_;
  FileMode mode_;
  FsCred creds_;
  FsCred entry_creds_;
  BTreeMap entries_;

 public:
  // impl<'a> StaticDirectoryBuilder<'a>
  static StaticDirectoryBuilder New(fbl::RefPtr<FileSystem> fs);

  void entry_creds(FsCred creds) { entry_creds_ = creds; }

  void entry(const CurrentTask& current_task, const char* name, ktl::unique_ptr<FsNodeOps> ops,
             FileMode mode);

  void entry_dev(const CurrentTask& current_task, const char* name, ktl::unique_ptr<FsNodeOps> ops,
                 FileMode mode, const DeviceType& dev);

  template <typename BuildSubdirFn>
  void subdir(const CurrentTask& current_task, const char* name, uint32_t mode,
              BuildSubdirFn&& build_subdir) {
    StaticDirectoryBuilder subdir(fs_);
    build_subdir(subdir);
    subdir.set_mode(FileMode::IFDIR);
    node(name, subdir.build(current_task));
  }

  void node(const char* name, FsNodeHandle node);

  void set_mode(FileMode mode) {
    if (!mode.is_dir()) {
    }
    mode_ = mode;
  }

  void dir_creds(FsCred creds) { creds_ = creds; }

  FsNodeHandle build(const CurrentTask& current_task);

  void build_root();

 private:
  StaticDirectoryBuilder(fbl::RefPtr<FileSystem> fs);
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_STATIC_DIRECTORY_H_
