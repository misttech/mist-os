// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <errno.h>
#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/ddk/platform-defs.h>
#include <lib/devmgr-integration-test/fixture.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/watcher.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zx/vmo.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zircon/status.h>

#include <zxtest/zxtest.h>

namespace {

using device_watcher::RecursiveWaitForFile;
using devmgr_integration_test::IsolatedDevmgr;

TEST(PbusTest, Enumeration) {
  // NB: this loop is never run. RealmBuilder::Build is in the call stack, and insists on a non-null
  // dispatcher.
  //
  // TODO(https://fxbug.dev/42065538): Remove this.
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  zx::result devmgr = IsolatedDevmgr::Create(
      {
          .root_device_driver = "fuchsia-boot:///platform-bus#meta/platform-bus.cm",
      },
      loop.dispatcher());
  ASSERT_OK(devmgr.status_value());

  const int dirfd = devmgr.value().devfs_root().get();

  ASSERT_OK(RecursiveWaitForFile(dirfd, "sys/platform").status_value());
  EXPECT_OK(RecursiveWaitForFile(dirfd, "sys/platform/pt/test-board").status_value());
  EXPECT_OK(RecursiveWaitForFile(dirfd, "sys/platform/test-parent").status_value());
  EXPECT_OK(RecursiveWaitForFile(dirfd, "sys/platform/test-parent/child-1").status_value());
  EXPECT_OK(RecursiveWaitForFile(dirfd, "sys/platform/test-parent/child-1/child-2").status_value());
  EXPECT_OK(RecursiveWaitForFile(dirfd, "sys/platform/test-parent/child-1/child-2/child-4")
                .status_value());
  EXPECT_OK(
      RecursiveWaitForFile(dirfd, "sys/platform/test-parent/child-1/child-3-top").status_value());
  EXPECT_OK(RecursiveWaitForFile(dirfd, "sys/platform/test-parent/child-1/child-3-top/child-3")
                .status_value());
  EXPECT_OK(RecursiveWaitForFile(dirfd, "sys/platform/gpio/test-gpio/gpio/gpio-3").status_value());
  EXPECT_OK(RecursiveWaitForFile(dirfd, "sys/platform/clock/test-clock/clock-1").status_value());
  EXPECT_OK(RecursiveWaitForFile(dirfd, "sys/platform/spi/test-spi/spi/spi-0-0").status_value());
  // TODO(316176095): Figure out why this driver binds but never starts.
  // EXPECT_EQ(RecursiveWaitForFile(dirfd,
  // "sys/platform/composite_node_spec/composite_node_spec").status_value(),
  //          ZX_OK);

  struct stat st;
  EXPECT_EQ(fstatat(dirfd, "sys/platform/pt/test-board", &st, 0), 0);
  EXPECT_EQ(fstatat(dirfd, "sys/platform/test-parent", &st, 0), 0);
  EXPECT_EQ(fstatat(dirfd, "sys/platform/test-parent/child-1", &st, 0), 0);
  EXPECT_EQ(fstatat(dirfd, "sys/platform/test-parent/child-1/child-2", &st, 0), 0);
  EXPECT_EQ(fstatat(dirfd, "sys/platform/test-parent/child-1/child-3-top", &st, 0), 0);
  EXPECT_EQ(fstatat(dirfd, "sys/platform/test-parent/child-1/child-2/child-4", &st, 0), 0);
  EXPECT_EQ(fstatat(dirfd, "sys/platform/test-parent/child-1/child-3-top/child-3", &st, 0), 0);
  EXPECT_EQ(fstatat(dirfd, "sys/platform/gpio/test-gpio/gpio/gpio-3", &st, 0), 0);
  EXPECT_EQ(fstatat(dirfd, "sys/platform/clock/test-clock/clock-1", &st, 0), 0);
  // TODO(316176095): Figure out why this driver binds but never starts.
  // EXPECT_EQ(
  //    fstatat(dirfd,
  //    "sys/platform/composite_node_spec/composite_node_spec/test-composite-node-spec", &st, 0),
  //    0);

  zx::result channel = RecursiveWaitForFile(dirfd, "sys/platform");
  ASSERT_OK(channel.status_value());

  fidl::ClientEnd<fuchsia_sysinfo::SysInfo> client_end(std::move(channel.value()));

  const fidl::WireSyncClient client(std::move(client_end));

  // Get board name.
  [&client]() {
    const fidl::WireResult result = client->GetBoardName();
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
    const std::string board_info{response.name.get()};
    EXPECT_STREQ(board_info, "driver-integration-test");
  }();

  // Get interrupt controller information.
  [&client]() {
    const fidl::WireResult result = client->GetInterruptControllerInfo();
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
    ASSERT_NOT_NULL(response.info.get());
  }();

  // Get board revision information.
  [&client]() {
    const fidl::WireResult result = client->GetBoardRevision();
    ASSERT_OK(result.status());
    const fidl::WireResponse response = result.value();
    ASSERT_OK(response.status);
    ASSERT_NE(response.revision, 0);
  }();
}

}  // namespace
