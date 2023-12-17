// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// We should only include the public header and not `delivery_blob_private.h` in these tests.
#include <filesystem>

#include <fbl/unique_fd.h>

#include "src/storage/blobfs/delivery_blob.h"
#include "src/storage/blobfs/test/blob_utils.h"
#include "src/storage/blobfs/test/integration/blobfs_fixtures.h"

namespace blobfs {

using fs_test::TestFilesystemOptions;

namespace {

std::string_view kNullBlobRoot = "15ec7bf0b50732b49f8228e07d24365338f9e3ab994b00af08e5a3bffe55fd8b";

// Blobs shared across test cases. The path of each entry is just the Merkle root.
const std::unique_ptr<BlobInfo> kTestBlobs[] = {
    GenerateRandomBlob("", 0ul),                      // Null Blob
    GenerateRandomBlob("", 1024ul),                   // Random smaller than 1 block
    GenerateRandomBlob("", kBlobfsBlockSize * 20ul),  // Random larger than 1 block
    GenerateRealisticBlob("", 1ul << 16),             // Realistic 64k blob
};

class DeliveryBlobIntegrationTest : public BaseBlobfsTest {
 protected:
  DeliveryBlobIntegrationTest() : BaseBlobfsTest(TestFilesystemOptions::DefaultBlobfs()) {}
};

// Verify we can write uncompressed delivery blobs.
TEST_F(DeliveryBlobIntegrationTest, WriteUncompressed) {
  for (const std::unique_ptr<BlobInfo>& blob_info : kTestBlobs) {
    const auto blob_path = std::filesystem::path(fs().mount_path()) / blob_info->path;
    const auto delivery_path = GetDeliveryBlobPath(blob_path.c_str());
    const auto delivery_data = GenerateDeliveryBlobType1(
        {blob_info->data.get(), blob_info->size_data}, /*compress=*/false);

    // Write delivery blob and verify payload.
    {
      fbl::unique_fd fd(open(delivery_path.c_str(), O_CREAT | O_RDWR, S_IRUSR | S_IWUSR));
      ASSERT_TRUE(fd) << strerror(errno) << delivery_path;
      ASSERT_EQ(ftruncate(fd.get(), delivery_data->size()), 0) << strerror(errno);
      ASSERT_EQ(StreamAll(write, fd.get(), delivery_data->data(), delivery_data->size()), 0)
          << strerror(errno);
      ASSERT_NO_FATAL_FAILURE(
          VerifyContents(fd.get(), blob_info->data.get(), blob_info->size_data));
      ASSERT_EQ(close(fd.release()), 0) << strerror(errno);
    }

    // Re-open and verify the blob's contents based on its Merkle root.
    {
      fbl::unique_fd fd(open(blob_path.c_str(), O_RDONLY));
      ASSERT_TRUE(fd) << strerror(errno);
      ASSERT_NO_FATAL_FAILURE(
          VerifyContents(fd.get(), blob_info->data.get(), blob_info->size_data));
    }
  }
}

// Verify we can write uncompressed delivery blobs.
TEST_F(DeliveryBlobIntegrationTest, WriteCompressed) {
  for (const std::unique_ptr<BlobInfo>& blob_info : kTestBlobs) {
    const auto blob_path = std::filesystem::path(fs().mount_path()) / blob_info->path;
    const auto delivery_path = GetDeliveryBlobPath(blob_path.c_str());
    const auto delivery_data =
        GenerateDeliveryBlobType1({blob_info->data.get(), blob_info->size_data}, /*compress=*/true);

    // Write delivery blob and verify payload.
    {
      fbl::unique_fd fd(open(delivery_path.c_str(), O_CREAT | O_RDWR, S_IRUSR | S_IWUSR));
      ASSERT_TRUE(fd) << strerror(errno);
      ASSERT_EQ(ftruncate(fd.get(), delivery_data->size()), 0) << strerror(errno);
      ASSERT_EQ(StreamAll(write, fd.get(), delivery_data->data(), delivery_data->size()), 0)
          << strerror(errno);
      ASSERT_NO_FATAL_FAILURE(
          VerifyContents(fd.get(), blob_info->data.get(), blob_info->size_data));
      ASSERT_EQ(close(fd.release()), 0) << strerror(errno);
    }

    // Re-open and verify the blob's contents based on its Merkle root.
    {
      fbl::unique_fd fd(open(blob_path.c_str(), O_RDONLY));
      ASSERT_TRUE(fd) << strerror(errno);
      ASSERT_NO_FATAL_FAILURE(
          VerifyContents(fd.get(), blob_info->data.get(), blob_info->size_data));
    }
  }
}

// Test opening of delivery blobs using different paths once written.
TEST_F(DeliveryBlobIntegrationTest, PathHandling) {
  const auto blob_path = std::filesystem::path(fs().mount_path()) / kNullBlobRoot;
  const auto delivery_path = GetDeliveryBlobPath(blob_path.c_str());

  // Generate & write delivery blob.
  {
    const auto delivery_data =
        GenerateDeliveryBlobType1({static_cast<const uint8_t*>(nullptr), 0}, /*compress=*/false);

    fbl::unique_fd fd(open(delivery_path.c_str(), O_CREAT | O_RDWR, S_IRUSR | S_IWUSR));
    ASSERT_TRUE(fd) << strerror(errno) << delivery_path;
    ASSERT_EQ(ftruncate(fd.get(), delivery_data->size()), 0) << strerror(errno);
    ASSERT_EQ(StreamAll(write, fd.get(), delivery_data->data(), delivery_data->size()), 0)
        << strerror(errno);
    ASSERT_EQ(close(fd.release()), 0) << strerror(errno);
  }

  // Ensure we can re-open based on hash.
  {
    fbl::unique_fd fd(open(blob_path.c_str(), O_RDONLY));
    ASSERT_TRUE(fd) << strerror(errno);
  }
  // Ensure we can re-open based on delivery path.
  {
    fbl::unique_fd fd(open(delivery_path.c_str(), O_RDONLY));
    ASSERT_TRUE(fd) << strerror(errno);
  }
}

}  // namespace
}  // namespace blobfs
