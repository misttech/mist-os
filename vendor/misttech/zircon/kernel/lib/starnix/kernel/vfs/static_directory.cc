// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/static_directory.h"

#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>

#include "fbl/alloc_checker.h"

namespace starnix {

StaticDirectoryBuilder StaticDirectoryBuilder::New(fbl::RefPtr<FileSystem> fs) {
  return StaticDirectoryBuilder(fs);
}

void StaticDirectoryBuilder::entry(const CurrentTask& current_task, const char* name,
                                   ktl::unique_ptr<FsNodeOps> ops, FileMode mode) {
  entry_dev(current_task, name, std::move(ops), mode, DeviceType::NONE);
}

void StaticDirectoryBuilder::entry_dev(const CurrentTask& current_task, const char* name,
                                       ktl::unique_ptr<FsNodeOps> ops, FileMode mode,
                                       const DeviceType& dev) {
  auto node = fs_->create_node(current_task, ktl::move(ops), [&](ino_t id) {
    auto info = FsNodeInfo::New(id, mode, entry_creds_);
    info.rdev = dev;
    return info;
  });
  this->node(name, node);
}

void StaticDirectoryBuilder::node(const char* name, FsNodeHandle node) {
  auto result = entries_.emplace(name, ktl::move(node));
  if (!result.second) {
    // throw std::runtime_error("adding a duplicate entry into a StaticDirectory");
  }
}

FsNodeHandle StaticDirectoryBuilder::build(const CurrentTask& current_task) {
  fbl::AllocChecker ac;
  StaticDirectory sd{ktl::move(entries_)};
  if (!ac.check()) {
    return fbl::RefPtr<FsNode>();
  }
  // return fs_->create_node(current_task, &sd, FsNodeInfo::new_factory(mode_, creds_));
  return FsNodeHandle();
}

void StaticDirectoryBuilder::build_root() {
  /*auto node = FsNode::new_root_with_properties(fbl::AdoptRef(new StaticDirectory{entries_}),
                                               [&](FsNodeInfo& info) {
                                                 info.uid = creds_.uid;
                                                 info.gid = creds_.gid;
                                                 info.mode = mode_;
                                               });
  fs_->set_root_node(node);*/
}

StaticDirectoryBuilder::StaticDirectoryBuilder(fbl::RefPtr<FileSystem> fs)
    : fs_(ktl::move(fs)),
      mode_(FILE_MODE(IFDIR, 0777)),
      creds_(FsCred::root()),
      entry_creds_(FsCred::root()) {}

}  // namespace starnix
