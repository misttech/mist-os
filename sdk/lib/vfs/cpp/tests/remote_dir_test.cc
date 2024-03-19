// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// These tests verify basic functionality of `vfs::RemoteDir`. For more comprehensive tests, see
// //src/storage/lib/vfs/cpp and //src/storage/conformance.

#include <fcntl.h>
#include <fuchsia/io/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/vfs/cpp/pseudo_dir.h>
#include <lib/vfs/cpp/remote_dir.h>
#include <lib/vfs/cpp/vmo_file.h>
#include <zircon/status.h>

#include <memory>

#include <fbl/unique_fd.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

namespace {

// Fixture sets up the following hierarchy:
//
//    root/
//      remote_dir/
//        file
//
class RemoteDirTest : public ::gtest::RealLoopFixture {
 protected:
  void SetUp() override {
    // Create a pseudo-directory with a file in it.
    remote_dir_ = std::make_unique<vfs::PseudoDir>();
    constexpr size_t kFileSize = 100;
    zx::vmo vmo;
    ASSERT_EQ(zx::vmo::create(kFileSize, 0, &vmo), ZX_OK);
    auto file = std::make_unique<vfs::VmoFile>(std::move(vmo), kFileSize);
    ASSERT_EQ(remote_dir_->AddEntry("file", std::move(file)), ZX_OK);
    // Serve the pseudo-dir and use it to create a remote node.
    zx::channel remote_client, remote_server;
    ASSERT_EQ(zx::channel::create(0, &remote_client, &remote_server), ZX_OK);
    ASSERT_EQ(remote_dir_->Serve(fuchsia::io::OpenFlags::RIGHT_READABLE, std::move(remote_server)),
              ZX_OK);
    auto remote_node = std::make_unique<vfs::RemoteDir>(std::move(remote_client));
    // Create the root directory and add the remote node.
    root_ = std::make_unique<vfs::PseudoDir>();
    ASSERT_EQ(root_->AddEntry("remote_dir", std::move(remote_node)), ZX_OK);
    zx::channel root_server;
    ASSERT_EQ(zx::channel::create(0, &root_client_, &root_server), ZX_OK);
    ASSERT_EQ(root_->Serve(
                  fuchsia::io::OpenFlags::RIGHT_READABLE | fuchsia::io::OpenFlags::RIGHT_WRITABLE,
                  std::move(root_server)),
              ZX_OK);
  }

  // Consumes and opens the root connection as a file descriptor. This must be called from a
  // different thread than the one serving the connection.
  fbl::unique_fd open_root_fd() {
    ZX_ASSERT_MSG(root_client_.is_valid(), "open_root_fd() can only be called once per test!");
    fbl::unique_fd fd;
    zx_status_t status = fdio_fd_create(root_client_.release(), fd.reset_and_get_address());
    ZX_ASSERT_MSG(status == ZX_OK, "Failed to create fd: %s", zx_status_get_string(status));
    return fd;
  }

 private:
  std::unique_ptr<vfs::PseudoDir> root_;
  std::unique_ptr<vfs::PseudoDir> remote_dir_;
  zx::channel root_client_;
};

TEST_F(RemoteDirTest, Open) {
  PerformBlockingWork([this] {
    auto root = open_root_fd();
    // We should be able to cross the remote mount point and open the file as read-only.
    fbl::unique_fd file;
    zx_status_t status =
        fdio_open_fd_at(root.get(), "remote_dir/file", O_RDONLY, file.reset_and_get_address());
    ASSERT_EQ(status, ZX_OK) << zx_status_get_string(status);
    // We should not be able to open the file as writable since the remote only has read access.
    status = fdio_open_fd_at(root.get(), "remote_dir/file", O_RDWR, file.reset_and_get_address());
    ASSERT_EQ(status, ZX_ERR_ACCESS_DENIED) << zx_status_get_string(status);
  });
}

}  // namespace
