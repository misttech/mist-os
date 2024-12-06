// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_MOUNTS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_MOUNTS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/util/error.h>

#include <fbl/ref_ptr.h>
#include <ktl/pair.h>

namespace starnix {

class CurrentTask;
class FileSystem;
class Kernel;
struct FileSystemOptions;

using FileSystemHandle = fbl::RefPtr<FileSystem>;

}  // namespace starnix

namespace starnix_kernel_runner {

using mtl::Error;

class MountAction {
 public:
  // The path where the filesystem should be mounted
  starnix::FsString path_;

  // The filesystem to mount
  starnix::FileSystemHandle fs_;

  // Mount flags
  starnix_uapi::MountFlags flags_;

  static fit::result<Error, MountAction> new_for_root(const fbl::RefPtr<starnix::Kernel>& kernel,
                                                      ktl::string_view spec);

  static fit::result<Error, MountAction> from_spec(const starnix::CurrentTask& current_task,
                                                   ktl::string_view spec);
};

class MountSpec {
 private:
  starnix::FsString mount_point_;
  starnix::FsString fs_type_;
  starnix_uapi::MountFlags flags_;

  // impl MountSpec
  static fit::result<mtl::Error, ktl::pair<MountSpec, starnix::FileSystemOptions>> parse(
      ktl::string_view spec);

  MountAction into_action(starnix::FileSystemHandle fs) const;

  // C++
  friend class MountAction;
  explicit MountSpec(ktl::string_view mount_point, ktl::string_view fs_type,
                     starnix_uapi::MountFlags flags)
      : mount_point_(mount_point), fs_type_(fs_type), flags_(flags) {}
};

}  // namespace starnix_kernel_runner

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_RUNNER_MOUNTS_H_
