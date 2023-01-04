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

#include <memory>

#include <fbl/ref_ptr.h>
#include <fbl/unique_fd.h>
#include <zxtest/zxtest.h>

#include "src/lib/storage/vfs/cpp/pseudo_dir.h"
#include "src/lib/storage/vfs/cpp/pseudo_file.h"
#include "src/lib/storage/vfs/cpp/synchronous_vfs.h"

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

  fs::SynchronousVfs vfs(loop.dispatcher());
  ASSERT_OK(vfs.ServeDirectory(first, std::move(server)));

  ASSERT_OK(loop.StartThread());

  fbl::unique_fd dir;
  ASSERT_OK(fdio_fd_create(client.TakeChannel().release(), dir.reset_and_get_address()));

  ASSERT_OK(device_watcher::RecursiveWaitForFile(dir.get(), "second/third/file"));
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

  fs::SynchronousVfs vfs(loop.dispatcher());
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

  fs::SynchronousVfs vfs(loop.dispatcher());
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
}

class IterateDirectoryTest : public zxtest::Test {
 protected:
  IterateDirectoryTest()
      : loop_(&kAsyncLoopConfigNoAttachToCurrentThread), vfs_(loop_.dispatcher()) {}

  void SetUp() override {
    // Set up the fake filesystem.
    auto file1 = fbl::MakeRefCounted<fs::UnbufferedPseudoFile>(
        [](fbl::String* output) { return ZX_OK; }, [](std::string_view input) { return ZX_OK; });
    auto file2 = fbl::MakeRefCounted<fs::UnbufferedPseudoFile>(
        [](fbl::String* output) { return ZX_OK; }, [](std::string_view input) { return ZX_OK; });

    auto first = fbl::MakeRefCounted<fs::PseudoDir>();
    ASSERT_OK(first->AddEntry("file1", file1));
    ASSERT_OK(first->AddEntry("file2", file2));

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints);
    auto& [client, server] = endpoints.value();

    ASSERT_OK(vfs_.ServeDirectory(first, std::move(server)));

    ASSERT_OK(loop_.StartThread());

    ASSERT_OK(fdio_fd_create(client.TakeChannel().release(), dir_.reset_and_get_address()));
  }

  void TearDown() override { loop_.Shutdown(); }

  const fbl::unique_fd& dir() { return dir_; }

 private:
  async::Loop loop_;
  fs::SynchronousVfs vfs_;
  fbl::unique_fd dir_;
};

TEST_F(IterateDirectoryTest, IterateDirectory) {
  std::vector<std::string> seen;
  zx_status_t status = device_watcher::IterateDirectory(
      dir().get(), [&seen](std::string_view filename, zx::channel channel) {
        // Collect the file names into the vector.
        seen.emplace_back(filename);
        return ZX_OK;
      });
  ASSERT_OK(status);

  // Make sure the file names seen were as expected.
  ASSERT_EQ(2, seen.size());
  std::sort(seen.begin(), seen.end());
  ASSERT_EQ("file1", seen[0]);
  ASSERT_EQ("file2", seen[1]);
}

TEST_F(IterateDirectoryTest, IterateDirectoryCancelled) {
  // Test that iteration is cancelled when the callback returns an error
  std::vector<std::string> seen;
  zx_status_t status = device_watcher::IterateDirectory(
      dir().get(), [&seen](std::string_view filename, zx::channel channel) {
        seen.emplace_back(filename);
        return ZX_ERR_INTERNAL;
      });
  ASSERT_STATUS(status, ZX_ERR_INTERNAL);

  // Should only have seen a single file before exiting.
  ASSERT_EQ(1, seen.size());
}

TEST_F(IterateDirectoryTest, IterateDirectoryChannel) {
  // Test that we can use the channel passed to the callback function to make
  // fuchsia.io.Node calls.
  std::vector<uint64_t> content_sizes;
  zx_status_t status = device_watcher::IterateDirectory(
      dir().get(), [&content_sizes](std::string_view filename, zx::channel channel) {
        auto result =
            fidl::WireCall(fidl::UnownedClientEnd<fuchsia_io::Node>(channel.borrow()))->GetAttr();
        if (!result.ok()) {
          return result.status();
        }
        content_sizes.push_back(result.value().attributes.content_size);
        return ZX_OK;
      });
  ASSERT_OK(status);

  ASSERT_EQ(2, content_sizes.size());

  // Files are empty.
  ASSERT_EQ(0, content_sizes[0]);
  ASSERT_EQ(0, content_sizes[1]);
}

}  // namespace
