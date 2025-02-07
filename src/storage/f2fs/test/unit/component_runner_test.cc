// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/component_runner.h"

#include <fidl/fuchsia.fs.startup/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.process.lifecycle/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/zx/resource.h>

#include <gtest/gtest.h>

#include "src/storage/f2fs/bcache.h"
#include "src/storage/f2fs/common.h"
#include "src/storage/f2fs/f2fs.h"
#include "src/storage/f2fs/mkfs.h"
#include "src/storage/f2fs/segment.h"
#include "src/storage/f2fs/vnode.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"

namespace f2fs {
namespace {

constexpr uint64_t kDefaultBlockCount =
    (kMinMetaSegments + kMinReservedSectionsForGc + kNrCursegType) * kDefaultBlocksPerSegment;
class F2fsComponentRunnerTest : public testing::Test {
 public:
  F2fsComponentRunnerTest() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {}

  void SetUp() override {
    auto device = std::make_unique<block_client::FakeBlockDevice>(kDefaultBlockCount, kBlockSize);
    auto bc_or = CreateBcacheMapper(std::move(device));
    bc_or = Mkfs(MkfsOptions{}, std::move(*bc_or));
    ASSERT_TRUE(bc_or.is_ok());
    bcache_ = std::move(*bc_or);

    auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
    root_ = std::move(endpoints.client);
    server_end_ = std::move(endpoints.server);
  }

  void StartServe() {
    runner_ = std::make_unique<ComponentRunner>(loop_.dispatcher());
    runner_->SetUnmountCallback([this]() { loop_.Quit(); });
    zx::result status = runner_->ServeRoot(std::move(server_end_), {});
    ASSERT_EQ(status.status_value(), ZX_OK);
  }

  fidl::ClientEnd<fuchsia_io::Directory> GetSvcDir() const {
    auto svc_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
    // TODO(https://fxbug.dev/378924259): Migrate to new Open signature.
    auto status = fidl::WireCall(root_)->DeprecatedOpen(
        fuchsia_io::wire::OpenFlags::kDirectory, {}, "svc",
        fidl::ServerEnd<fuchsia_io::Node>(svc_endpoints.server.TakeChannel()));
    EXPECT_EQ(status.status(), ZX_OK);
    return std::move(svc_endpoints.client);
  }

  fidl::ClientEnd<fuchsia_io::Directory> GetRootDir() const {
    auto root_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
    // TODO(https://fxbug.dev/378924259): Migrate to new Open signature.
    auto status = fidl::WireCall(root_)->DeprecatedOpen(
        fuchsia_io::wire::OpenFlags::kRightReadable | fuchsia_io::wire::OpenFlags::kRightWritable |
            fuchsia_io::wire::OpenFlags::kDirectory,
        {}, "root", fidl::ServerEnd<fuchsia_io::Node>(root_endpoints.server.TakeChannel()));
    EXPECT_EQ(status.status(), ZX_OK);
    return std::move(root_endpoints.client);
  }

  void ResetRootDir() { root_.reset(); }

 protected:
  async::Loop loop_;
  std::unique_ptr<BcacheMapper> bcache_;
  std::unique_ptr<ComponentRunner> runner_;
  fidl::ClientEnd<fuchsia_io::Directory> root_;
  fidl::ServerEnd<fuchsia_io::Directory> server_end_;
};

TEST_F(F2fsComponentRunnerTest, ServeAndConfigureStartsF2fs) {
  ASSERT_NO_FATAL_FAILURE(StartServe());

  auto svc_dir = GetSvcDir();
  auto client_end = component::ConnectAt<fuchsia_fs_startup::Startup>(svc_dir.borrow());
  ASSERT_EQ(client_end.status_value(), ZX_OK);

  MountOptions options;
  zx::result status = runner_->Configure(std::move(bcache_), options);
  ASSERT_EQ(status.status_value(), ZX_OK);

  std::atomic<bool> callback_called = false;
  runner_->Shutdown([callback_called = &callback_called](zx_status_t status) {
    EXPECT_EQ(status, ZX_OK);
    *callback_called = true;
  });
  // Shutdown quits the loop.
  ASSERT_EQ(loop_.Run(), ZX_ERR_CANCELED);
  ASSERT_TRUE(callback_called);
}

TEST_F(F2fsComponentRunnerTest, ShutdownWithoutF2fs) {
  ASSERT_NO_FATAL_FAILURE(StartServe());
  runner_->SetUnmountCallback([]() {});
  runner_->Shutdown([](zx_status_t status) { EXPECT_EQ(status, ZX_OK); });
  loop_.RunUntilIdle();
}

TEST_F(F2fsComponentRunnerTest, OnNoConnections) {
  ASSERT_NO_FATAL_FAILURE(StartServe());
  ResetRootDir();
  loop_.RunUntilIdle();
  zx::nanosleep(zx::deadline_after(zx::sec(1)));
  ASSERT_TRUE(runner_->IsTerminating());
}

TEST_F(F2fsComponentRunnerTest, RequestsBeforeStartupAreQueuedAndServicedAfter) {
  // Start a call to the filesystem. We expect that this request will be queued and won't return
  // until Configure is called on the runner. Initially, GetRootDir will fire off an open call on
  // the root_ connection, but as the server end isn't serving anything yet, the request is queued
  // there. Once root_ starts serving requests, and the svc dir exists, (which is done by
  // StartServe below) that open call succeeds, but the root itself should be waiting to serve any
  // open calls it gets, queuing any requests. Once Configure is called, the root should start
  // servicing requests, and the request will succeed.
  auto root_dir = GetRootDir();
  fidl::WireSharedClient<fuchsia_io::Directory> root_client(std::move(root_dir),
                                                            loop_.dispatcher());

  std::atomic<bool> query_complete = false;
  root_client->QueryFilesystem().ThenExactlyOnce(
      [query_complete =
           &query_complete](fidl::WireUnownedResult<fuchsia_io::Directory::QueryFilesystem>& info) {
        EXPECT_EQ(info.status(), ZX_OK);
        EXPECT_EQ(info->s, ZX_OK);
        *query_complete = true;
      });
  ASSERT_EQ(loop_.RunUntilIdle(), ZX_OK);
  ASSERT_FALSE(query_complete);

  ASSERT_NO_FATAL_FAILURE(StartServe());
  ASSERT_EQ(loop_.RunUntilIdle(), ZX_OK);
  ASSERT_FALSE(query_complete);

  auto svc_dir = GetSvcDir();
  auto client_end = component::ConnectAt<fuchsia_fs_startup::Startup>(svc_dir.borrow());
  ASSERT_EQ(client_end.status_value(), ZX_OK);

  MountOptions options;
  zx::result status = runner_->Configure(std::move(bcache_), options);
  ASSERT_EQ(status.status_value(), ZX_OK);
  ASSERT_EQ(loop_.RunUntilIdle(), ZX_OK);
  ASSERT_TRUE(query_complete);

  std::atomic<bool> callback_called = false;
  runner_->Shutdown([callback_called = &callback_called](zx_status_t status) {
    EXPECT_EQ(status, ZX_OK);
    *callback_called = true;
  });
  ASSERT_EQ(loop_.Run(), ZX_ERR_CANCELED);
  ASSERT_TRUE(callback_called);
}

TEST_F(F2fsComponentRunnerTest, LifecycleChannelShutsDownRunner) {
  auto lifecycle_endpoints = fidl::Endpoints<fuchsia_process_lifecycle::Lifecycle>::Create();
  auto lifecycle = std::move(lifecycle_endpoints.client);

  runner_ = std::make_unique<ComponentRunner>(loop_.dispatcher());
  std::atomic<bool> unmount_callback_called = false;
  runner_->SetUnmountCallback([this, &unmount_callback_called]() {
    EXPECT_FALSE(unmount_callback_called);
    loop_.Quit();
    unmount_callback_called = true;
  });
  zx::result status =
      runner_->ServeRoot(std::move(server_end_), std::move(lifecycle_endpoints.server));
  ASSERT_EQ(status.status_value(), ZX_OK);
  ASSERT_EQ(loop_.RunUntilIdle(), ZX_OK);
  ASSERT_FALSE(unmount_callback_called);

  MountOptions options;
  status = runner_->Configure(std::move(bcache_), options);
  ASSERT_EQ(status.status_value(), ZX_OK);
  ASSERT_EQ(loop_.RunUntilIdle(), ZX_OK);
  ASSERT_FALSE(unmount_callback_called);

  auto lifecycle_stop_res = fidl::WireCall(lifecycle)->Stop();
  ASSERT_EQ(lifecycle_stop_res.status(), ZX_OK);

  ASSERT_EQ(loop_.Run(), ZX_ERR_CANCELED);
  ASSERT_TRUE(unmount_callback_called);
}

}  // namespace
}  // namespace f2fs
