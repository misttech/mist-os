// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/service/ota_health_check.h"

#include <gtest/gtest.h>

#include "src/storage/blobfs/blob.h"
#include "src/storage/blobfs/blobfs.h"
#include "src/storage/blobfs/format.h"
#include "src/storage/blobfs/mkfs.h"
#include "src/storage/blobfs/mount.h"
#include "src/storage/blobfs/test/blob_utils.h"
#include "src/storage/blobfs/test/blobfs_test_setup.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "zircon/syscalls.h"

namespace blobfs {
namespace {

namespace fuv = ::fuchsia_update_verify;

constexpr uint32_t kBlockSize = 512;
constexpr uint32_t kNumBlocks = 400 * kBlobfsBlockSize / kBlockSize;

class OtaHealthCheckServiceTest : public testing::Test {
 protected:
  void SetUp() override {
    EXPECT_EQ(ZX_OK, setup_.CreateFormatMount(kNumBlocks, kBlockSize));
    svc_ = fbl::MakeRefCounted<OtaHealthCheckService>(setup_.dispatcher(), *setup_.blobfs());
  }

  void InstallBlob(const BlobInfo& info) {
    auto root = OpenRoot();
    zx::result file = root->Create(info.path, fs::CreationType::kFile);
    ASSERT_TRUE(file.is_ok()) << file.status_string();
    size_t out_actual = 0;
    ASSERT_EQ(file->Truncate(info.size_data), ZX_OK);
    ASSERT_EQ(file->Write(info.data.get(), info.size_data, 0, &out_actual), ZX_OK);
    ASSERT_EQ(out_actual, info.size_data);

    file->Close();
  }

  void CorruptBlob(const BlobInfo& info) {
    ZX_ASSERT(info.size_data);
    uint64_t block;
    {
      auto root = OpenRoot();
      fbl::RefPtr<fs::Vnode> file;
      ASSERT_EQ(root->Lookup(info.path, &file), ZX_OK);
      auto blob = fbl::RefPtr<Blob>::Downcast(file);
      block = setup_.blobfs()->GetNode(blob->Ino())->extents[0].Start() +
              DataStartBlock(setup_.blobfs()->Info());
    }

    // Unmount.
    std::unique_ptr<block_client::BlockDevice> device = setup_.Unmount();

    // Read the block that contains the blob.
    storage::VmoBuffer buffer;
    ASSERT_EQ(buffer.Initialize(device.get(), 1, kBlobfsBlockSize, "test_buffer"), ZX_OK);
    block_fifo_request_t request = {
        .command = {.opcode = BLOCK_OPCODE_READ, .flags = 0},
        .vmoid = buffer.vmoid(),
        .length = kBlobfsBlockSize / kBlockSize,
        .vmo_offset = 0,
        .dev_offset = block * kBlobfsBlockSize / kBlockSize,
    };
    ASSERT_EQ(device->FifoTransaction(&request, 1), ZX_OK);

    // Flip a byte.
    uint8_t* target = static_cast<uint8_t*>(buffer.Data(0));
    *target ^= 0xff;

    // Write the block back.
    request.command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0};
    ASSERT_EQ(device->FifoTransaction(&request, 1), ZX_OK);

    // Remount and try and read the blob.
    EXPECT_EQ(ZX_OK, setup_.Mount(std::move(device)));
    svc_ = fbl::MakeRefCounted<OtaHealthCheckService>(setup_.dispatcher(), *setup_.blobfs());
  }

  fbl::RefPtr<fs::Vnode> OpenRoot() {
    fbl::RefPtr<fs::Vnode> root;
    EXPECT_EQ(setup_.blobfs()->OpenRootNode(&root), ZX_OK);
    return root;
  }

  fidl::WireSyncClient<fuv::ComponentOtaHealthCheck> Client() {
    auto endpoints = fidl::Endpoints<fuv::ComponentOtaHealthCheck>::Create();
    EXPECT_EQ(svc_->ConnectService(endpoints.server.TakeChannel()), ZX_OK);
    return fidl::WireSyncClient(std::move(endpoints.client));
  }

  BlobfsTestSetupWithThread setup_;
  fbl::RefPtr<OtaHealthCheckService> svc_;  // References setup_.blobfs().
};

TEST_F(OtaHealthCheckServiceTest, EmptyFilesystemPassesChecks) {
  fidl::WireSyncClient<fuv::ComponentOtaHealthCheck> client = Client();
  auto result = client->GetHealthStatus();
  ASSERT_TRUE(result.ok()) << result.error();
}

TEST_F(OtaHealthCheckServiceTest, PopulatedFilesystemPassesChecks) {
  // Since only open files are validated, open a bunch of valid files.
  std::vector<fbl::RefPtr<fs::Vnode>> files;
  auto root = OpenRoot();
  for (int i = 0; i < 10; ++i) {
    std::unique_ptr<BlobInfo> info = GenerateRandomBlob("", 65536);
    InstallBlob(*info);

    auto& file = files.emplace_back();
    EXPECT_EQ(root->Lookup(info->path, &file), ZX_OK);
    EXPECT_EQ(file->Open(nullptr), ZX_OK);
  }

  fidl::WireSyncClient<fuv::ComponentOtaHealthCheck> client = Client();
  auto result = client->GetHealthStatus();
  ASSERT_TRUE(result.ok()) << result.error();
  EXPECT_EQ(result->health_status, fuv::HealthStatus::kHealthy);

  // Balance out the Open() calls above so the node can clean up properly.
  for (auto& file : files) {
    file->Close();
  }
}

TEST_F(OtaHealthCheckServiceTest, NullBlobPassesChecks) {
  std::unique_ptr<BlobInfo> info = GenerateRandomBlob("", 0);
  InstallBlob(*info);

  auto root = OpenRoot();
  fbl::RefPtr<fs::Vnode> file;
  ASSERT_EQ(root->Lookup(info->path, &file), ZX_OK);
  ASSERT_EQ(file->Open(nullptr), ZX_OK);

  fidl::WireSyncClient<fuv::ComponentOtaHealthCheck> client = Client();
  auto result = client->GetHealthStatus();
  ASSERT_TRUE(result.ok()) << result.error();
  EXPECT_EQ(result->health_status, fuv::HealthStatus::kHealthy);
  // Balance out the Open() call above so the node can clean up properly.
  file->Close();
}

TEST_F(OtaHealthCheckServiceTest, InvalidFileFailsChecks) {
  std::unique_ptr<BlobInfo> info = GenerateRandomBlob("", 65536);
  InstallBlob(*info);
  CorruptBlob(*info);

  auto root = OpenRoot();
  fbl::RefPtr<fs::Vnode> file;
  ASSERT_EQ(root->Lookup(info->path, &file), ZX_OK);
  ASSERT_EQ(file->Open(nullptr), ZX_OK);

  fidl::WireSyncClient<fuv::ComponentOtaHealthCheck> client = Client();
  auto result = client->GetHealthStatus();
  ASSERT_TRUE(result.ok()) << result.error();
  EXPECT_EQ(result->health_status, fuv::HealthStatus::kUnhealthy);
  // Balance out the Open() call above so the node can clean up properly.
  file->Close();
}

TEST_F(OtaHealthCheckServiceTest, InvalidButClosedFilePassesChecks) {
  std::unique_ptr<BlobInfo> info = GenerateRandomBlob("", 65536);
  InstallBlob(*info);
  CorruptBlob(*info);

  fidl::WireSyncClient<fuv::ComponentOtaHealthCheck> client = Client();
  auto result = client->GetHealthStatus();
  ASSERT_TRUE(result.ok()) << result.error();
  EXPECT_EQ(result->health_status, fuv::HealthStatus::kHealthy);
}

}  // namespace
}  // namespace blobfs
