// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>

#include <utility>

#include <sanitizer/lsan_interface.h>
#include <zxtest/zxtest.h>

#include "src/storage/lib/vfs/cpp/connection/connection.h"
#include "src/storage/lib/vfs/cpp/fuchsia_vfs.h"
#include "src/storage/lib/vfs/cpp/pseudo_dir.h"

namespace {

// Base class used to define fake Vfs objects to test |Connection::Bind|.
class NoOpVfs : public fs::FuchsiaVfs {
 public:
  using FuchsiaVfs::FuchsiaVfs;

 protected:
  fbl::DoublyLinkedList<std::unique_ptr<fs::internal::Connection>> connections_;

 private:
  void Shutdown(ShutdownCallback handler) override { FAIL("Should never be reached in this test"); }
  bool IsTerminating() const final {
    ADD_FAILURE("Should never be reached in this test");
    return false;
  }
  void CloseAllConnectionsForVnode(const fs::Vnode& node,
                                   CloseAllConnectionsForVnodeCallback callback) final {
    FAIL("Should never be reached in this test");
  }
};

// A Vfs that first places connections into a linked list before
// starting message dispatch.
class NoOpVfsGood : public NoOpVfs {
 public:
  using NoOpVfs::NoOpVfs;

 private:
  zx::result<> RegisterConnection(std::unique_ptr<fs::internal::Connection> connection,
                                  zx::channel& server_end) final {
    connections_.push_back(std::move(connection));
    connections_.back().Bind(std::move(server_end), [](fs::internal::Connection*) {});
    return zx::ok();
  }
};

// A Vfs that first starts message dispatch on a connection before placing it into a linked list.
// This behavior is racy (https://fxbug.dev/42122489) so we test that it triggers a failed
// precondition check.
class NoOpVfsBad : public NoOpVfs {
 public:
  using NoOpVfs::NoOpVfs;

 private:
  zx::result<> RegisterConnection(std::unique_ptr<fs::internal::Connection> connection,
                                  zx::channel& server_end) final {
    connection->Bind(std::move(server_end), [](fs::internal::Connection*) {});
    connections_.push_back(std::move(connection));
    return zx::ok();
  }
};

template <typename VfsType>
void RunTest(async::Loop* loop, VfsType&& vfs) {
  auto root = fbl::MakeRefCounted<fs::PseudoDir>();
  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ASSERT_OK(endpoints.status_value());

  ASSERT_OK(vfs.ServeDirectory(root, std::move(endpoints->server), fuchsia_io::kRStarDir));
  loop->RunUntilIdle();
}

TEST(ConnectionTest, BindRequiresVfsManagingConnection_Positive) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  RunTest(&loop, NoOpVfsGood(loop.dispatcher()));
}

TEST(ConnectionDeathTest, BindRequiresVfsManagingConnection_Negative) {
  // Bind requires registering the connection in a list first.
  if (ZX_DEBUG_ASSERT_IMPLEMENTED) {
    async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
    ASSERT_DEATH([&] {
#if __has_feature(address_sanitizer) || __has_feature(leak_sanitizer) || \
    __has_feature(hwaddress_sanitizer)
      // Disable LSAN, this thread is expected to leak by way of a crash.
      __lsan::ScopedDisabler _;
#endif
      RunTest(&loop, NoOpVfsBad(loop.dispatcher()));
    });
  }
}

}  // namespace
