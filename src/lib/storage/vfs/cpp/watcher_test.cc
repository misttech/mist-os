// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/storage/vfs/cpp/watcher.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <fbl/ref_ptr.h>
#include <zxtest/zxtest.h>

#include "src/lib/storage/vfs/cpp/managed_vfs.h"
#include "src/lib/storage/vfs/cpp/pseudo_dir.h"
#include "src/lib/storage/vfs/cpp/vfs_types.h"

namespace {

namespace fio = fuchsia_io;

class WatcherTest : public zxtest::Test {
 public:
  WatcherTest()
      : loop_(&kAsyncLoopConfigAttachToCurrentThread),
        vfs_(loop_.dispatcher()),
        root_(fbl::MakeRefCounted<fs::PseudoDir>()) {}

  async::Loop& loop() { return loop_; }
  fbl::RefPtr<fs::PseudoDir>& root() { return root_; }

  fidl::ClientEnd<fio::DirectoryWatcher> WatchRootDir(fio::WatchMask mask) {
    auto endpoints = fidl::CreateEndpoints<fio::DirectoryWatcher>();
    EXPECT_OK(endpoints);
    auto [client, server] = *std::move(endpoints);
    EXPECT_OK(root_->WatchDir(&vfs_, mask, 0, std::move(server)));
    return std::move(client);
  }

 protected:
  void TearDown() override { ASSERT_OK(loop_.RunUntilIdle()); }

 private:
  async::Loop loop_;
  fs::ManagedVfs vfs_;
  fbl::RefPtr<fs::PseudoDir> root_;
};

TEST_F(WatcherTest, WatchersDroppedOnChannelClosed) {
  ASSERT_FALSE(root()->HasWatchers());
  {
    fidl::ClientEnd client = WatchRootDir(fio::WatchMask::kAdded);
    ASSERT_TRUE(root()->HasWatchers());
  }
  ASSERT_OK(loop().RunUntilIdle());
  ASSERT_FALSE(root()->HasWatchers());
}

}  // namespace
