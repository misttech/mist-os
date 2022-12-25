// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/storage/vfs/cpp/synchronous_vfs.h"

#include <lib/async/cpp/task.h>
#include <lib/sync/completion.h>

#include <memory>
#include <utility>

namespace fs {

SynchronousVfs::SynchronousVfs(async_dispatcher_t* dispatcher) : FuchsiaVfs(dispatcher) {}

SynchronousVfs::~SynchronousVfs() {
  Shutdown(nullptr);
  ZX_DEBUG_ASSERT(connections_ == nullptr);
}

void SynchronousVfs::Shutdown(ShutdownCallback handler) {
  std::for_each(connections_->begin(), connections_->end(),
                std::mem_fn(&internal::Connection::Unbind));
  connections_.reset();
  if (handler) {
    handler(ZX_OK);
  }
}

void SynchronousVfs::CloseAllConnectionsForVnode(const Vnode& node,
                                                 CloseAllConnectionsForVnodeCallback callback) {
  for (internal::Connection& connection : *connections_) {
    if (connection.vnode().get() == &node) {
      connection.Unbind();
    }
  }
  if (callback) {
    callback();
  }
}

zx_status_t SynchronousVfs::RegisterConnection(std::unique_ptr<internal::Connection> connection,
                                               zx::channel channel) {
  ZX_DEBUG_ASSERT(connections_ != nullptr);
  connections_->push_back(std::move(connection));
  connections_->back().StartDispatching(
      std::move(channel), [weak = std::weak_ptr(connections_)](internal::Connection* connection) {
        if (std::shared_ptr connections = weak.lock(); connections != nullptr) {
          connections->erase(*connection);
        }
      });
  return ZX_OK;
}

bool SynchronousVfs::IsTerminating() const { return connections_ == nullptr; }

}  // namespace fs
