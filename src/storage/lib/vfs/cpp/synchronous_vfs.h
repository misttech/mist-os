// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_SYNCHRONOUS_VFS_H_
#define SRC_STORAGE_LIB_VFS_CPP_SYNCHRONOUS_VFS_H_

#ifndef __Fuchsia__
#error "Fuchsia-only header"
#endif

#include <lib/async/cpp/task.h>
#include <lib/async/dispatcher.h>
#include <lib/zx/channel.h>
#include <zircon/compiler.h>

#include <memory>

#include <fbl/intrusive_double_list.h>

#include "src/storage/lib/vfs/cpp/connection.h"
#include "src/storage/lib/vfs/cpp/fuchsia_vfs.h"

namespace fs {

// A specialization of |FuchsiaVfs| which can be trivially dropped.
//
// During its destruction connections are *usually* destroyed on the destroying thread, though this
// is inherently racy with channel-initiated teardown, meaning that connections may be torn down on
// the dispatcher thread as well. Connections may even outlive the SynchronousVfs in such cases.
//
// This class is NOT thread-safe and it must be used with a single-threaded asynchronous dispatcher.
class SynchronousVfs : public FuchsiaVfs {
 public:
  explicit SynchronousVfs(async_dispatcher_t* dispatcher = nullptr);

  // The SynchronousVfs destructor terminates all open connections.
  ~SynchronousVfs() override;

  // FuchsiaVfs overrides.
  void CloseAllConnectionsForVnode(const Vnode& node,
                                   CloseAllConnectionsForVnodeCallback callback) final;
  bool IsTerminating() const final;

 private:
  // Synchronously drop all connections managed by the VFS.
  //
  // Invokes |handler| once when all connections are destroyed. It is safe to delete SynchronousVfs
  // from within the closure.
  void Shutdown(ShutdownCallback handler) override;

  zx_status_t RegisterConnection(std::unique_ptr<internal::Connection> connection,
                                 zx::channel channel) final;

  struct Connections {
    std::mutex lock;
    fbl::DoublyLinkedList<std::unique_ptr<internal::Connection>> inner __TA_GUARDED(lock);
  };

  // Held by shared_ptr to allow connections to unbind asynchronously even as the VFS is being
  // destroyed. Reset when the VFS is shutting down.
  std::shared_ptr<Connections> connections_{std::make_shared<Connections>()};
};

}  // namespace fs

#endif  // SRC_STORAGE_LIB_VFS_CPP_SYNCHRONOUS_VFS_H_
