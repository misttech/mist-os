// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_REGISTRY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_REGISTRY_H_

#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/btree_map.h>
#include <lib/mistos/util/error_propagation.h>

#include <fbl/ref_ptr.h>
#include <ktl/string_view.h>

namespace starnix {

using CreateFs =
    std::function<fit::result<Errno, FileSystemHandle>(const CurrentTask&, FileSystemOptions)>;

class FsRegistry : public fbl::RefCounted<FsRegistry> {
 private:
  starnix_sync::Mutex<util::BTreeMap<ktl::string_view, CreateFs>> registry_;

 public:
  void register_fs(ktl::string_view fs_type, CreateFs create_fs) {
    auto locked = registry_.Lock();
    auto [_, inserted] = locked->try_emplace(fs_type, ktl::move(create_fs));
    ZX_ASSERT(inserted);
  }

  fit::result<Errno, FileSystemHandle> create(const CurrentTask& current_task,
                                              ktl::string_view fs_type, FileSystemOptions options) {
    auto locked = registry_.Lock();
    auto it = locked->find(fs_type);
    if (it == locked->end()) {
      return fit::error(errno(ENODEV));
    }

    auto fs = it->second(current_task, ktl::move(options)) _EP(fs);

    // TODO (Herrera): Implement security::file_system_resolve_security

    return fit::ok(fs.value());
  }
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_REGISTRY_H_
