// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"

#include <lib/async/cpp/task.h>

#include <memory>
#include <mutex>

#include "src/storage/lib/vfs/cpp/connection/connection.h"

namespace fs {

SynchronousVfs::SynchronousVfs(async_dispatcher_t* dispatcher) : FuchsiaVfs(dispatcher) {}

SynchronousVfs::~SynchronousVfs() { Shutdown(nullptr); }

void SynchronousVfs::Shutdown(ShutdownCallback handler) {
  // Arrange for connections to be unbound asynchronously.
  {
    std::lock_guard lock(vfs_lock_);
    for (internal::Connection& connection : connections_) {
      connection.Unbind();
    }
    connections_.clear();
    WillDestroy();
  }

  WaitTillDone();

  if (handler)
    handler(ZX_OK);
}

void SynchronousVfs::CloseAllConnectionsForVnode(const Vnode& node,
                                                 CloseAllConnectionsForVnodeCallback callback) {
  {
    std::lock_guard lock(vfs_lock_);
    for (internal::Connection& connection : connections_) {
      if (connection.vnode().get() == &node) {
        connection.Unbind();
      }
    }
  }
  if (callback) {
    callback();
  }
}

zx::result<> SynchronousVfs::RegisterConnection(std::unique_ptr<internal::Connection> connection,
                                                zx::channel& channel) {
  internal::Connection* ptr;

  {
    std::lock_guard lock(vfs_lock_);
    if (IsTerminating())
      return zx::error(ZX_ERR_CANCELED);
    ptr = connection.get();
    connections_.push_back(connection.release());
  }

  ptr->Bind(std::move(channel), [](internal::Connection* connection) {
    auto vfs = connection->vfs();
    if (vfs) {
      SynchronousVfs* sync_vfs = static_cast<SynchronousVfs*>(vfs.get());
      std::lock_guard lock(sync_vfs->vfs_lock_);
      if (!vfs->IsTerminating())
        sync_vfs->connections_.erase(*connection);
    }
    delete connection;
  });

  return zx::ok();
}

}  // namespace fs
