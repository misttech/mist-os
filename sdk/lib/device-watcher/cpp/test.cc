// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/fd.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/sync/cpp/completion.h>

#include <memory>

#include <fbl/ref_ptr.h>
#include <fbl/unique_fd.h>
#include <zxtest/zxtest.h>

#include "src/lib/storage/vfs/cpp/managed_vfs.h"
#include "src/lib/storage/vfs/cpp/pseudo_dir.h"
#include "src/lib/storage/vfs/cpp/pseudo_file.h"

namespace {

TEST(DeviceWatcherTest, Smoke) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  auto file = fbl::MakeRefCounted<fs::UnbufferedPseudoFile>(
      [](fbl::String* output) { return ZX_OK; }, [](std::string_view input) { return ZX_OK; });

  auto third = fbl::MakeRefCounted<fs::PseudoDir>();
  ASSERT_OK(third->AddEntry("file", file));

  auto second = fbl::MakeRefCounted<fs::PseudoDir>();
  ASSERT_OK(second->AddEntry("third", std::move(third)));

  auto first = fbl::MakeRefCounted<fs::PseudoDir>();
  ASSERT_OK(first->AddEntry("second", std::move(second)));
  ASSERT_OK(first->AddEntry("file", file));

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ASSERT_OK(endpoints);
  auto& [client, server] = endpoints.value();

  fs::ManagedVfs vfs(loop.dispatcher());
  ASSERT_OK(vfs.ServeDirectory(first, std::move(server)));

  ASSERT_OK(loop.StartThread());

  fbl::unique_fd dir;
  ASSERT_OK(fdio_fd_create(client.TakeChannel().release(), dir.reset_and_get_address()));

  ASSERT_OK(device_watcher::RecursiveWaitForFile(dir.get(), "second/third/file"));

  libsync::Completion shutdown_complete;
  vfs.Shutdown([&shutdown_complete](zx_status_t status) {
    EXPECT_OK(status);
    shutdown_complete.Signal();
  });

  shutdown_complete.Wait();
}

TEST(DeviceWatcherTest, OpenInNamespace) {
  ASSERT_OK(device_watcher::RecursiveWaitForFile("/dev/sys/test"));
  ASSERT_STATUS(device_watcher::RecursiveWaitForFile("/other-test/file"), ZX_ERR_NOT_FOUND);
}

TEST(DeviceWatcherTest, DirWatcherWaitForRemoval) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  auto file = fbl::MakeRefCounted<fs::UnbufferedPseudoFile>(
      [](fbl::String* output) { return ZX_OK; }, [](std::string_view input) { return ZX_OK; });

  auto third = fbl::MakeRefCounted<fs::PseudoDir>();
  ASSERT_OK(third->AddEntry("file", file));

  auto second = fbl::MakeRefCounted<fs::PseudoDir>();
  ASSERT_OK(second->AddEntry("third", third));

  auto first = fbl::MakeRefCounted<fs::PseudoDir>();
  ASSERT_OK(first->AddEntry("second", second));
  ASSERT_OK(first->AddEntry("file", file));

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ASSERT_OK(endpoints);
  auto& [client, server] = endpoints.value();

  fs::ManagedVfs vfs(loop.dispatcher());
  ASSERT_OK(vfs.ServeDirectory(first, std::move(server)));

  ASSERT_OK(loop.StartThread());

  fbl::unique_fd dir;
  ASSERT_OK(fdio_fd_create(client.TakeChannel().release(), dir.reset_and_get_address()));
  fbl::unique_fd sub_dir(openat(dir.get(), "second/third", O_DIRECTORY | O_RDONLY));

  ASSERT_OK(device_watcher::RecursiveWaitForFile(dir.get(), "second/third/file"));

  // Verify removal of the root directory file
  std::unique_ptr<device_watcher::DirWatcher> root_watcher;
  ASSERT_OK(device_watcher::DirWatcher::Create(dir.get(), &root_watcher));

  ASSERT_OK(first->RemoveEntry("file"));
  ASSERT_OK(root_watcher->WaitForRemoval("file", zx::duration::infinite()));

  // Verify removal of the subdirectory file
  std::unique_ptr<device_watcher::DirWatcher> sub_watcher;
  ASSERT_OK(device_watcher::DirWatcher::Create(sub_dir.get(), &sub_watcher));

  ASSERT_OK(third->RemoveEntry("file"));
  ASSERT_OK(sub_watcher->WaitForRemoval("file", zx::duration::infinite()));

  libsync::Completion shutdown_complete;
  vfs.Shutdown([&shutdown_complete](zx_status_t status) {
    EXPECT_OK(status);
    shutdown_complete.Signal();
  });

  shutdown_complete.Wait();
}

TEST(DeviceWatcherTest, DirWatcherVerifyUnowned) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  auto file = fbl::MakeRefCounted<fs::UnbufferedPseudoFile>(
      [](fbl::String* output) { return ZX_OK; }, [](std::string_view input) { return ZX_OK; });

  auto first = fbl::MakeRefCounted<fs::PseudoDir>();
  ASSERT_OK(first->AddEntry("file", file));

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ASSERT_OK(endpoints);
  auto& [client, server] = endpoints.value();

  fs::ManagedVfs vfs(loop.dispatcher());
  ASSERT_OK(vfs.ServeDirectory(first, std::move(server)));

  ASSERT_OK(loop.StartThread());

  fbl::unique_fd dir;
  ASSERT_OK(fdio_fd_create(client.TakeChannel().release(), dir.reset_and_get_address()));

  std::unique_ptr<device_watcher::DirWatcher> root_watcher;
  ASSERT_OK(device_watcher::DirWatcher::Create(dir.get(), &root_watcher));

  // Close the directory fd
  ASSERT_OK(dir.reset());

  // Verify the watcher can still successfully wait for removal
  ASSERT_OK(first->RemoveEntry("file"));
  ASSERT_OK(root_watcher->WaitForRemoval("file", zx::duration::infinite()));

  libsync::Completion shutdown_complete;
  vfs.Shutdown([&shutdown_complete](zx_status_t status) {
    EXPECT_OK(status);
    shutdown_complete.Signal();
  });

  shutdown_complete.Wait();
}

}  // namespace
