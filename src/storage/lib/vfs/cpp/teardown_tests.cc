// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/errors.h>

#include <fbl/ref_ptr.h>
#include <zxtest/zxtest.h>

#include "src/storage/lib/vfs/cpp/managed_vfs.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace {

class FdCountVnode : public fs::Vnode {
 public:
  FdCountVnode() = default;
  ~FdCountVnode() override {
    std::lock_guard lock(mutex_);
    EXPECT_EQ(0, open_count());
  }

  size_t fds() const {
    std::lock_guard lock(mutex_);
    return open_count();
  }

  fs::VnodeProtocolSet GetProtocols() const final { return fs::VnodeProtocol::kFile; }

  zx_status_t GetNodeInfoForProtocol([[maybe_unused]] fs::VnodeProtocol protocol,
                                     [[maybe_unused]] fs::Rights rights,
                                     fs::VnodeRepresentation* info) override {
    *info = fs::VnodeRepresentation::Connector();
    return ZX_OK;
  }
};

struct AsyncTearDownSync {
  libsync::Completion sync_thread_start;
  libsync::Completion sync_may_proceed;
  libsync::Completion sync_vnode_destroyed;
};

class AsyncTearDownVnode : public FdCountVnode {
 public:
  explicit AsyncTearDownVnode(AsyncTearDownSync& completions, zx_status_t status_for_sync = ZX_OK)
      : completions_(completions), status_for_sync_(status_for_sync) {}

  ~AsyncTearDownVnode() override {
    // C) Tear down the Vnode.
    EXPECT_EQ(0, fds());
    if (thread_.has_value()) {
      thread_.value().join();
    }
    completions_.sync_vnode_destroyed.Signal();
  }

 private:
  void Sync(fs::Vnode::SyncCallback callback) final {
    thread_.emplace([&completions = this->completions_, callback = std::move(callback),
                     status_for_sync = status_for_sync_]() mutable {
      // A) Identify when the sync has started being processed.
      completions.sync_thread_start.Signal();
      // B) Wait until the connection has been closed.
      completions.sync_may_proceed.Wait();
      callback(status_for_sync);
    });
  }

  std::optional<std::thread> thread_;
  AsyncTearDownSync& completions_;
  zx_status_t status_for_sync_;
};

// Helper function which creates a VFS with a served Vnode, starts a sync request, and then closes
// the connection to the client in the middle of the async callback.
//
// This helps tests get ready to try handling a tricky teardown.
void SyncStart(AsyncTearDownSync& completions, async::Loop* loop,
               std::unique_ptr<fs::ManagedVfs>* vfs, zx_status_t status_for_sync = ZX_OK) {
  *vfs = std::make_unique<fs::ManagedVfs>(loop->dispatcher());
  ASSERT_OK(loop->StartThread());

  auto vn = fbl::AdoptRef(new AsyncTearDownVnode(completions, status_for_sync));
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Node>();
  ASSERT_OK(endpoints.status_value());
  auto& [client_end, server_end] = endpoints.value();
  ASSERT_OK(vn->OpenValidating({}, nullptr));
  ASSERT_OK((*vfs)->Serve(vn, server_end.TakeChannel(), {}));
  vn = nullptr;

  fidl::WireClient client(std::move(client_end), loop->dispatcher());
  client->Sync().ThenExactlyOnce([](fidl::WireUnownedResult<fuchsia_io::Node::Sync>& result) {
    ASSERT_FALSE(result.ok());
    ASSERT_TRUE(result.is_canceled());
  });

  // A) Wait for sync to begin.
  completions.sync_thread_start.Wait();
}

void CommonTestUnpostedTeardown(zx_status_t status_for_sync) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  AsyncTearDownSync completions;
  std::unique_ptr<fs::ManagedVfs> vfs;

  ASSERT_NO_FAILURES(SyncStart(completions, &loop, &vfs, status_for_sync));

  // B) Let sync complete.
  completions.sync_may_proceed.Signal();

  libsync::Completion shutdown_done;
  vfs->Shutdown([&](zx_status_t status) {
    EXPECT_OK(status);
    // C) Issue an explicit shutdown, check that the Vnode has
    // already torn down.
    EXPECT_OK(completions.sync_vnode_destroyed.Wait(zx::duration::infinite_past()));
    shutdown_done.Signal();
  });
  ASSERT_OK(shutdown_done.Wait(zx::sec(3)));
}

// Test a case where the VFS object is shut down outside the dispatch loop.
TEST(Teardown, UnpostedTeardown) { CommonTestUnpostedTeardown(ZX_OK); }

// Test a case where the VFS object is shut down outside the dispatch loop, where the |Vnode::Sync|
// operation also failed causing the connection to be closed.
TEST(Teardown, UnpostedTeardownSyncError) { CommonTestUnpostedTeardown(ZX_ERR_INVALID_ARGS); }

void CommonTestPostedTeardown(zx_status_t status_for_sync) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  AsyncTearDownSync completions;
  std::unique_ptr<fs::ManagedVfs> vfs;

  ASSERT_NO_FAILURES(SyncStart(completions, &loop, &vfs, status_for_sync));

  // B) Let sync complete.
  completions.sync_may_proceed.Signal();

  libsync::Completion shutdown_done;
  ASSERT_OK(async::PostTask(loop.dispatcher(), [&]() {
    vfs->Shutdown([&](zx_status_t status) {
      EXPECT_OK(status);
      // C) Issue an explicit shutdown, check that the Vnode has
      // already torn down.
      EXPECT_OK(completions.sync_vnode_destroyed.Wait(zx::duration::infinite_past()));
      shutdown_done.Signal();
    });
  }));
  ASSERT_OK(shutdown_done.Wait(zx::sec(3)));
}

// Test a case where the VFS object is shut down as a posted request to the dispatch loop.
TEST(Teardown, PostedTeardown) { ASSERT_NO_FAILURES(CommonTestPostedTeardown(ZX_OK)); }

// Test a case where the VFS object is shut down as a posted request to the dispatch loop, where the
// |Vnode::Sync| operation also failed causing the connection to be closed.
TEST(Teardown, PostedTeardownSyncError) {
  ASSERT_NO_FAILURES(CommonTestPostedTeardown(ZX_ERR_INVALID_ARGS));
}

// Test a case where the VFS object destroyed inside the callback to Shutdown.
TEST(Teardown, TeardownDeleteThis) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  AsyncTearDownSync completions;
  std::unique_ptr<fs::ManagedVfs> vfs;

  ASSERT_NO_FAILURES(SyncStart(completions, &loop, &vfs));

  // B) Let sync complete.
  completions.sync_may_proceed.Signal();

  libsync::Completion shutdown_done;
  fs::ManagedVfs* raw_vfs = vfs.release();
  raw_vfs->Shutdown([&](zx_status_t status) {
    EXPECT_OK(status);
    // C) Issue an explicit shutdown, check that the Vnode has already torn down.
    EXPECT_OK(completions.sync_vnode_destroyed.Wait(zx::duration::infinite_past()));
    delete raw_vfs;
    shutdown_done.Signal();
  });
  ASSERT_OK(shutdown_done.Wait(zx::sec(3)));
}

TEST(Teardown, SynchronousTeardown) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  ASSERT_OK(loop.StartThread());
  zx::channel client;

  {
    // Tear down the VFS while the async loop is running.
    auto vfs = std::make_unique<fs::SynchronousVfs>(loop.dispatcher());
    auto vn = fbl::AdoptRef(new FdCountVnode());
    zx::channel server;
    ASSERT_OK(zx::channel::create(0, &client, &server));
    ASSERT_OK(vn->OpenValidating({}, nullptr));
    ASSERT_OK(vfs->Serve(vn, std::move(server), {}));
  }

  loop.Quit();

  {
    // Tear down the VFS while the async loop is not running.
    auto vfs = std::make_unique<fs::SynchronousVfs>(loop.dispatcher());
    auto vn = fbl::AdoptRef(new FdCountVnode());
    zx::channel server;
    ASSERT_OK(zx::channel::create(0, &client, &server));
    ASSERT_OK(vn->OpenValidating({}, nullptr));
    ASSERT_OK(vfs->Serve(vn, std::move(server), {}));
  }

  {
    // Tear down the VFS with no active connections.
    auto vfs = std::make_unique<fs::SynchronousVfs>(loop.dispatcher());
  }
}

}  // namespace
