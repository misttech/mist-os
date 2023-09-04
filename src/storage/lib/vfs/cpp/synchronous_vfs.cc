// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"

#include <lib/async/cpp/task.h>
#include <lib/sync/completion.h>

#include <memory>
#include <mutex>
#include <utility>

namespace fs {

SynchronousVfs::SynchronousVfs(async_dispatcher_t* dispatcher) : FuchsiaVfs(dispatcher) {}

SynchronousVfs::~SynchronousVfs() {
  Shutdown(nullptr);
  ZX_DEBUG_ASSERT(connections_ == nullptr);
}

void SynchronousVfs::Shutdown(ShutdownCallback handler) {
  ZX_DEBUG_ASSERT(connections_ != nullptr);
  {
    std::lock_guard<std::mutex> lock(connections_->lock);
    std::for_each(connections_->inner.begin(), connections_->inner.end(),
                  std::mem_fn(&internal::Connection::Unbind));
  }
  connections_.reset();
  if (handler) {
    handler(ZX_OK);
  }
}

void SynchronousVfs::CloseAllConnectionsForVnode(const Vnode& node,
                                                 CloseAllConnectionsForVnodeCallback callback) {
  ZX_DEBUG_ASSERT(connections_ != nullptr);
  {
    std::lock_guard<std::mutex> lock(connections_->lock);
    for (internal::Connection& connection : connections_->inner) {
      if (connection.vnode().get() == &node) {
        connection.Unbind();
      }
    }
  }
  if (callback) {
    callback();
  }
}

zx_status_t SynchronousVfs::RegisterConnection(std::unique_ptr<internal::Connection> connection,
                                               zx::channel channel) {
  ZX_DEBUG_ASSERT(connections_ != nullptr);
  // Release the lock before doing additional work on the connection.
  internal::Connection& added = [&]() -> internal::Connection& {
    std::lock_guard<std::mutex> lock(connections_->lock);
    connections_->inner.push_back(std::move(connection));
    return connections_->inner.back();
  }();

  // Subtle behavior warning.
  //
  // Connections must not outlive the VFS; this is because they may call into the VFS during normal
  // operations, including during their destructors.
  //
  // Callbacks are provided weak references to the connections container as a best-effort attempt to
  // ensure this; destroying the VFS drops the strong reference, nullifying the callback.
  //
  // However if a callback has already upgraded its reference when the VFS is destroyed then a race
  // results; the VFS attempts to iterate over the connections to call
  // `internal::Connection::Unbind` at the same time that (a) the connections container is being
  // modified and (b) a particular connection is being destroyed. Access to the connections
  // container is guarded by a lock to mitigate this.
  added.StartDispatching(std::move(channel),
                         [weak = std::weak_ptr(connections_)](internal::Connection* connection) {
                           if (std::shared_ptr connections = weak.lock(); connections != nullptr) {
                             // Release the lock before dropping the connection.
                             std::unique_ptr removed = [&]() {
                               std::lock_guard<std::mutex> lock(connections->lock);
                               return connections->inner.erase(*connection);
                             }();
                           }
                         });
  return ZX_OK;
}

bool SynchronousVfs::IsTerminating() const { return connections_ == nullptr; }

}  // namespace fs
