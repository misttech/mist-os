// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.component/cpp/wire.h>
#include <fidl/fuchsia.fs.startup/cpp/wire.h>
#include <fidl/fuchsia.fs/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>

#include <gtest/gtest.h>

#include "src/storage/testing/ram_disk.h"

namespace f2fs {
namespace {

constexpr uint32_t kBlockCount = 1024 * 256;
constexpr uint32_t kBlockSize = 512;

const fuchsia_component_decl::wire::ChildRef kF2fsChildRef{.name = "test-f2fs",
                                                           .collection = "fs-collection"};

class F2fsComponentTest : public testing::Test {
 public:
  void SetUp() override {
    auto realm_client_end = component::Connect<fuchsia_component::Realm>();
    ASSERT_EQ(realm_client_end.status_value(), ZX_OK);
    realm_ = fidl::WireSyncClient(std::move(*realm_client_end));

    fidl::Arena allocator;
    fuchsia_component_decl::wire::CollectionRef collection_ref{.name = "fs-collection"};
    auto child_decl = fuchsia_component_decl::wire::Child::Builder(allocator)
                          .name("test-f2fs")
                          .url("#meta/f2fs.cm")
                          .startup(fuchsia_component_decl::wire::StartupMode::kLazy)
                          .Build();
    fuchsia_component::wire::CreateChildArgs child_args;
    auto create_res = realm_->CreateChild(collection_ref, child_decl, child_args);
    ASSERT_EQ(create_res.status(), ZX_OK);
    ASSERT_FALSE(create_res->is_error())
        << "create error: " << static_cast<uint32_t>(create_res->error_value());

    auto exposed_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
    auto open_exposed_res =
        realm_->OpenExposedDir(kF2fsChildRef, std::move(exposed_endpoints.server));
    ASSERT_EQ(open_exposed_res.status(), ZX_OK);
    ASSERT_FALSE(open_exposed_res->is_error())
        << "open exposed dir error: " << static_cast<uint32_t>(open_exposed_res->error_value());
    exposed_dir_ = std::move(exposed_endpoints.client);

    auto startup_client_end =
        component::ConnectAt<fuchsia_fs_startup::Startup>(exposed_dir_.borrow());
    ASSERT_EQ(startup_client_end.status_value(), ZX_OK);
    startup_client_ = fidl::WireSyncClient(std::move(*startup_client_end));
  }

  void TearDown() override {
    auto destroy_res = realm_->DestroyChild(kF2fsChildRef);
    ASSERT_EQ(destroy_res.status(), ZX_OK);
    ASSERT_FALSE(destroy_res->is_error())
        << "destroy error: " << static_cast<uint32_t>(destroy_res->error_value());
  }

  void CreateRamDisk(int block_size = kBlockSize, uint64_t block_count = kBlockCount,
                     const storage::RamDisk::Options& options = storage::RamDisk::Options{}) {
    auto ramdisk_or = storage::RamDisk::Create(block_size, block_count);
    ASSERT_EQ(ramdisk_or.status_value(), ZX_OK);
    ramdisk_ = std::move(*ramdisk_or);
  }

  const fidl::WireSyncClient<fuchsia_fs_startup::Startup>& startup_client() const {
    return startup_client_;
  }

  fidl::UnownedClientEnd<fuchsia_io::Directory> exposed_dir() const {
    return exposed_dir_.borrow();
  }

  fidl::ClientEnd<fuchsia_hardware_block::Block> block_client() const {
    zx::result block_client_end =
        component::Connect<fuchsia_hardware_block::Block>(ramdisk_.path().c_str());
    EXPECT_TRUE(block_client_end.is_ok()) << block_client_end.status_string();
    const fidl::WireResult result = fidl::WireCall(block_client_end.value())->GetInfo();
    EXPECT_TRUE(result.ok()) << result.FormatDescription();
    const fit::result response = result.value();
    EXPECT_TRUE(response.is_ok()) << zx_status_get_string(response.error_value());
    return std::move(block_client_end.value());
  }

 private:
  storage::RamDisk ramdisk_;
  fidl::WireSyncClient<fuchsia_component::Realm> realm_;
  fidl::WireSyncClient<fuchsia_fs_startup::Startup> startup_client_;
  fidl::ClientEnd<fuchsia_io::Directory> exposed_dir_;
};

TEST_F(F2fsComponentTest, FormatCheckStart) {
  CreateRamDisk();
  fuchsia_fs_startup::wire::FormatOptions format_options;
  auto format_res = startup_client()->Format(block_client(), format_options);
  ASSERT_EQ(format_res.status(), ZX_OK);
  ASSERT_FALSE(format_res->is_error());

  fuchsia_fs_startup::wire::CheckOptions check_options;
  auto check_res = startup_client()->Check(block_client(), check_options);
  ASSERT_EQ(check_res.status(), ZX_OK);
  ASSERT_FALSE(check_res->is_error());

  fuchsia_fs_startup::wire::StartOptions start_options;
  auto startup_res = startup_client()->Start(block_client(), start_options);
  ASSERT_EQ(startup_res.status(), ZX_OK);
  ASSERT_FALSE(startup_res->is_error());

  auto admin_client_end = component::ConnectAt<fuchsia_fs::Admin>(exposed_dir());
  ASSERT_EQ(admin_client_end.status_value(), ZX_OK);
  fidl::WireSyncClient admin_client{std::move(*admin_client_end)};

  auto shutdown_res = admin_client->Shutdown();
  ASSERT_EQ(shutdown_res.status(), ZX_OK);
}

TEST_F(F2fsComponentTest, RequestsBeforeStartupAreQueuedAndServicedAfter) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread("f2fs caller test thread");

  CreateRamDisk();
  fuchsia_fs_startup::wire::FormatOptions format_options;
  auto format_res = startup_client()->Format(block_client(), format_options);
  ASSERT_EQ(format_res.status(), ZX_OK);
  ASSERT_FALSE(format_res->is_error());

  fuchsia_fs_startup::wire::CheckOptions check_options;
  auto check_res = startup_client()->Check(block_client(), check_options);
  ASSERT_EQ(check_res.status(), ZX_OK);
  ASSERT_FALSE(check_res->is_error());

  fuchsia_fs_startup::wire::StartOptions start_options;
  auto startup_res = startup_client()->Start(block_client(), start_options);
  ASSERT_EQ(startup_res.status(), ZX_OK);
  ASSERT_FALSE(startup_res->is_error());

  auto admin_client_end = component::ConnectAt<fuchsia_fs::Admin>(exposed_dir());
  ASSERT_EQ(admin_client_end.status_value(), ZX_OK);
  fidl::WireSyncClient admin_client{std::move(*admin_client_end)};

  auto shutdown_res = admin_client->Shutdown();
  ASSERT_EQ(shutdown_res.status(), ZX_OK);
}

TEST_F(F2fsComponentTest, SmallDiskException) {
  CreateRamDisk(kBlockSize, 1);

  fuchsia_fs_startup::wire::FormatOptions format_options;
  auto format_res = startup_client()->Format(block_client(), format_options);
  ASSERT_EQ(format_res.status(), ZX_OK);
  ASSERT_TRUE(format_res->is_error());
  ASSERT_EQ(format_res->error_value(), ZX_ERR_NO_SPACE);

  fuchsia_fs_startup::wire::CheckOptions check_options;
  auto check_res = startup_client()->Check(block_client(), check_options);
  ASSERT_EQ(check_res.status(), ZX_OK);
  ASSERT_TRUE(check_res->is_error());
  ASSERT_EQ(check_res->error_value(), ZX_ERR_NO_SPACE);

  fuchsia_fs_startup::wire::StartOptions start_options;
  auto startup_res = startup_client()->Start(block_client(), start_options);
  ASSERT_EQ(startup_res.status(), ZX_OK);
  ASSERT_TRUE(startup_res->is_error());
  ASSERT_EQ(startup_res->error_value(), ZX_ERR_NO_SPACE);
}

TEST_F(F2fsComponentTest, FormatFailureException) {
  uint64_t block_count = 20ull * 1024ull * 1024ull / kBlockSize;
  CreateRamDisk(kBlockSize, block_count);

  fuchsia_fs_startup::wire::FormatOptions format_options;
  auto format_res = startup_client()->Format(block_client(), format_options);
  ASSERT_EQ(format_res.status(), ZX_OK);
  ASSERT_TRUE(format_res->is_error());
  ASSERT_EQ(format_res->error_value(), ZX_ERR_NO_SPACE);
}

TEST_F(F2fsComponentTest, NoFormatDiskException) {
  CreateRamDisk();

  fuchsia_fs_startup::wire::CheckOptions check_options;
  auto check_res = startup_client()->Check(block_client(), check_options);
  ASSERT_EQ(check_res.status(), ZX_OK);
  ASSERT_TRUE(check_res->is_error());
  ASSERT_EQ(check_res->error_value(), ZX_ERR_NOT_FOUND);

  fuchsia_fs_startup::wire::StartOptions start_options;
  auto startup_res = startup_client()->Start(block_client(), start_options);
  ASSERT_EQ(startup_res.status(), ZX_OK);
  ASSERT_TRUE(startup_res->is_error());
  ASSERT_EQ(startup_res->error_value(), ZX_ERR_INVALID_ARGS);
}

}  // namespace
}  // namespace f2fs
