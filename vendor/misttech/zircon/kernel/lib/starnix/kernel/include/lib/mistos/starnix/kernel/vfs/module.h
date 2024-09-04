// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MODULE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MODULE_H_

#include <lib/mistos/starnix/kernel/vfs/fd_table.h>

namespace starnix {

class FileObject;
class CurrentTask;

using FileHandle = fbl::RefPtr<FileObject>;

// Service to handle delayed releases.
//
// Delayed releases are cleanup code that is run at specific point where the lock level is
// known. The starnix kernel must ensure that delayed releases are run regularly.
class DelayedReleaser {
 public:
  void flush_file(FileHandle file, FdTableId id) const;

  // Run all current delayed releases for the current thread.
  void apply(const CurrentTask& current_task) const;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MODULE_H_
