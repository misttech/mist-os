// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// These tests verify basic functionality of `vfs::Service`. For more comprehensive tests, see
// //src/storage/lib/vfs/cpp and //src/storage/conformance.

#include <fcntl.h>
#include <fuchsia/io/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/vfs/cpp/pseudo_dir.h>
#include <lib/vfs/cpp/service.h>
#include <zircon/status.h>

#include <memory>

#include <fbl/unique_fd.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

namespace {

constexpr size_t kMaxConnections = 3;

// Sets up a service at root/instance that drops connections after a certain limit is reached.
class ServiceTest : public ::gtest::RealLoopFixture {
 protected:
  void SetUp() override {
    root_ = std::make_unique<vfs::PseudoDir>();
    auto connector = [this](zx::channel channel, async_dispatcher_t* dispatcher) {
      if (channels_.size() < kMaxConnections) {
        channels_.push_back(std::move(channel));
      }
    };
    root_->AddEntry("instance", std::make_unique<vfs::Service>(std::move(connector)));
    zx::channel root_server;
    ASSERT_EQ(zx::channel::create(0, &root_client_, &root_server), ZX_OK);
    ASSERT_EQ(root_->Serve(
                  fuchsia::io::OpenFlags::RIGHT_READABLE | fuchsia::io::OpenFlags::RIGHT_WRITABLE,
                  std::move(root_server)),
              ZX_OK);
  }

  const zx::channel& root_client() { return root_client_; }

 private:
  std::unique_ptr<vfs::PseudoDir> root_;
  zx::channel root_client_;
  std::vector<zx::channel> channels_;
};

TEST_F(ServiceTest, Connect) {
  std::vector<zx::channel> channels;
  PerformBlockingWork([this, &channels] {
    for (size_t i = 0; i < (2 * kMaxConnections); ++i) {
      zx::channel client, server;
      ASSERT_EQ(zx::channel::create(0, &client, &server), ZX_OK);
      ASSERT_EQ(fdio_service_connect_at(root_client().get(), "instance", server.release()), ZX_OK);
      channels.push_back(std::move(client));
    }
  });

  RunLoopUntilIdle();
  constexpr std::string_view kData = "test";
  for (size_t i = 0; i < (2 * kMaxConnections); ++i) {
    zx_status_t status = channels[i].write(0, kData.data(), kData.size(), nullptr, 0);
    // We should succeed writing data into the channels that we kept alive in the test fixture, but
    // fail for the ones we closed the channels of.
    ASSERT_EQ(status, i < kMaxConnections ? ZX_OK : ZX_ERR_PEER_CLOSED);
  }
}

}  // namespace
