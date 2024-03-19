// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fuchsia/io/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/vfs/cpp/composed_service_dir.h>
#include <lib/vfs/cpp/pseudo_dir.h>
#include <lib/vfs/cpp/service.h>
#include <zircon/status.h>

#include <memory>
#include <string>
#include <string_view>

#include <fbl/unique_fd.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

namespace {

constexpr std::string_view kServices[]{"service_a", "service_b"};
constexpr std::string_view kFallbackServices[]{"service_b", "service_c"};
constexpr size_t kServiceConnections = 5;
constexpr size_t kFallbackServiceConnections = 2;

// Fixture sets up a composed service directory containing "service_a" and "service_b" which is
// backed by a fallback directory containing "service_b" and "service_c".
class ComposedServiceDirTest : public ::gtest::RealLoopFixture {
// vfs::ComposedServiceDir is deprecated. See https://fxbug.dev/309685624 for details.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
 protected:
  void SetUp() override {
    root_ = std::make_unique<vfs::ComposedServiceDir>();

    for (const auto service : kServices) {
      root_->AddService(std::string(service),
                        std::make_unique<vfs::Service>(MakeService(service, /*fallback*/ false)));
    }

    fallback_dir_ = std::make_unique<vfs::PseudoDir>();
    for (const auto service : kFallbackServices) {
      fallback_dir_->AddEntry(std::string(service), std::make_unique<vfs::Service>(
                                                        MakeService(service, /*fallback*/ true)));
    }

    zx::channel fallback_client, fallback_server;
    ASSERT_EQ(zx::channel::create(0, &fallback_client, &fallback_server), ZX_OK);
    ASSERT_EQ(fallback_dir_->Serve(
                  fuchsia::io::OpenFlags::RIGHT_READABLE | fuchsia::io::OpenFlags::RIGHT_WRITABLE,
                  std::move(fallback_server)),
              ZX_OK);
    root_->set_fallback(fidl::InterfaceHandle<fuchsia::io::Directory>{std::move(fallback_client)});

    zx::channel root_server;
    ASSERT_EQ(zx::channel::create(0, &root_client_, &root_server), ZX_OK);
    ASSERT_EQ(root_->Serve(
                  fuchsia::io::OpenFlags::RIGHT_READABLE | fuchsia::io::OpenFlags::RIGHT_WRITABLE,
                  std::move(root_server)),
              ZX_OK);
  }

  const zx::channel& root_client() { return root_client_; }

  auto& connection_attempts() { return connection_attempts_; }
  auto& fallback_attempts() { return fallback_attempts_; }

 private:
  fit::function<void(zx::channel channel, async_dispatcher_t* dispatcher)> MakeService(
      std::string_view name, bool fallback) {
    return [this, name, fallback](zx::channel channel, async_dispatcher_t* dispatcher) {
      if (fallback) {
        fallback_attempts_[name]++;
      } else {
        connection_attempts_[name]++;
      }
    };
  }

  std::unique_ptr<vfs::ComposedServiceDir> root_;
  std::unique_ptr<vfs::PseudoDir> fallback_dir_;
  zx::channel root_client_;
  std::map<std::string_view, size_t, std::less<>> connection_attempts_;
  std::map<std::string_view, size_t, std::less<>> fallback_attempts_;
#pragma clang diagnostic pop
};

TEST_F(ComposedServiceDirTest, Connect) {
  PerformBlockingWork([this] {
    for (size_t i = 0; i < kServiceConnections; ++i) {
      for (const auto& service : kServices) {
        zx::channel client, server;
        ASSERT_EQ(zx::channel::create(0, &client, &server), ZX_OK);
        ASSERT_EQ(fdio_service_connect_at(root_client().get(), service.data(), server.release()),
                  ZX_OK);
      }
    }
    for (size_t i = 0; i < kFallbackServiceConnections; ++i) {
      for (const auto& service : kFallbackServices) {
        zx::channel client, server;
        ASSERT_EQ(zx::channel::create(0, &client, &server), ZX_OK);
        ASSERT_EQ(fdio_service_connect_at(root_client().get(), service.data(), server.release()),
                  ZX_OK);
      }
    }
  });

  RunLoopUntilIdle();

  // service_a is only in the root, service_b is in both, and service_c is only in the fallback.

  EXPECT_EQ(connection_attempts()["service_a"], kServiceConnections);
  EXPECT_EQ(connection_attempts()["service_b"], kServiceConnections + kFallbackServiceConnections);
  EXPECT_EQ(connection_attempts()["service_c"], 0u);

  EXPECT_EQ(fallback_attempts()["service_a"], 0u);
  EXPECT_EQ(fallback_attempts()["service_b"], 0u);
  EXPECT_EQ(fallback_attempts()["service_c"], kFallbackServiceConnections);
}

}  // namespace
