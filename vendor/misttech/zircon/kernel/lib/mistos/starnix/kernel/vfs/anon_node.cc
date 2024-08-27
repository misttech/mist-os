// Copyright 2024 Mist Tecnlogia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/anon_node.h"

#include <lib/mistos/starnix/kernel/task/module.h>
#include <lib/mistos/starnix/kernel/vfs/module.h>

namespace starnix {

FileHandle Anon::new_file_extended(const CurrentTask& current_task, ktl::unique_ptr<FileOps> ops,
                                   OpenFlags flags, std::function<FsNodeInfo(ino_t)> info) {
  fbl::AllocChecker ac;
  auto anon = new (&ac) Anon();
  ASSERT(ac.check());

  auto fs = anon_fs(current_task->kernel());
  return FileObject::new_anonymous(
      std::move(ops), fs->create_node(current_task, ktl::unique_ptr<FsNodeOps>(anon), info), flags);
}

FileHandle Anon::new_file(const CurrentTask& current_task, ktl::unique_ptr<FileOps> ops,
                          OpenFlags flags) {
  return new_file_extended(
      current_task, std::move(ops), flags,
      FsNodeInfo::new_factory(FileMode::from_bits(0600), current_task->as_fscred()));
}

FileSystemHandle anon_fs(const fbl::RefPtr<Kernel>& kernel) {
  if (!kernel->anon_fs.is_initialized()) {
    fbl::AllocChecker ac;
    auto anonfs = new (&ac) AnonFs();
    ASSERT(ac.check());
    kernel->anon_fs.set(FileSystem::New(kernel, {CacheModeType::Uncached}, anonfs, {}));
  }
  return kernel->anon_fs.get();
}

}  // namespace starnix
