// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACE_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/namespace_node.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/vfs.h>

#include <optional>
#include <utility>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <kernel/mutex.h>

namespace starnix {

class Mount;
class FileSystem;

using MountHandle = fbl::RefPtr<Mount>;
using FileSystemHandle = fbl::RefPtr<FileSystem>;

// A mount namespace.
//
// The namespace records at which entries filesystems are mounted.
class Namespace : public fbl::RefCounted<Namespace> {
 private:
  MountHandle root_mount_;

 public:
  // Unique ID of this namespace.
  uint64_t id_;

 public:
  // impl Namespace

  static fbl::RefPtr<Namespace> New(FileSystemHandle fs);

  static fbl::RefPtr<Namespace> new_with_flags(FileSystemHandle fs, MountFlags flags);

  NamespaceNode root();

 public:
  // C++
  ~Namespace();

 private:
  Namespace(MountHandle root_mount, uint64_t id);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_NAMESPACE_H_
