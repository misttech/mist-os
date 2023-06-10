// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_SCOPED_MEMFS_MANAGER_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_SCOPED_MEMFS_MANAGER_H_

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/sync/completion.h>
#include <lib/syslog/cpp/macros.h>

#include <future>
#include <string>
#include <unordered_map>
#include <vector>

#include "src/storage/memfs/mounted_memfs.h"

namespace forensics::testing {

// Manages creating and destroying MemFs backed directories in the calling processes namespace.
class ScopedMemFsManager {
 public:
  ScopedMemFsManager() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {}

  ~ScopedMemFsManager() {
    std::vector<std::promise<zx_status_t>> promises;
    promises.reserve(filesystems_.size());

    for (const auto& [_, memfs] : filesystems_) {
      std::promise<zx_status_t>& promise = promises.emplace_back();
      memfs->Shutdown([&promise](zx_status_t status) { promise.set_value(status); });
    }

    for (auto& promise : promises) {
      FX_CHECK(promise.get_future().get() == ZX_OK);
    }

    loop_.Shutdown();
  }

  bool Contains(const std::string& path) const { return filesystems_.count(path) != 0; }

  // Create a memfs backed directory at |path| in the component's namespace.
  void Create(const std::string& path) {
    FX_CHECK(!Contains(path));
    if (!started_) {
      FX_CHECK(loop_.StartThread("forensics-scoped-memfs-manager") == ZX_OK);
      started_ = true;
    }
    zx::result memfs = MountedMemfs::Create(loop_.dispatcher(), path.c_str());
    FX_CHECK(memfs.is_ok()) << memfs.status_string();
    filesystems_.emplace(path, std::move(memfs.value()));
  }

  // Destroy the memfs backed directory at |path| in the component's namespace.
  void Destroy(const std::string& path) {
    auto nh = filesystems_.extract(path);
    FX_CHECK(!nh.empty());
    std::promise<zx_status_t> promise;
    nh.mapped()->Shutdown([&promise](zx_status_t status) { promise.set_value(status); });
    FX_CHECK(promise.get_future().get() == ZX_OK);
  }

 private:
  std::unordered_map<std::string, MountedMemfs> filesystems_;
  async::Loop loop_;
  bool started_{false};
};

}  // namespace forensics::testing

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_SCOPED_MEMFS_MANAGER_H_
