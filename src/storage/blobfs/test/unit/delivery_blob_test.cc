// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/delivery_blob.h"

#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <memory>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/storage/blobfs/blobfs.h"
#include "src/storage/blobfs/common.h"
#include "src/storage/blobfs/format.h"
#include "src/storage/blobfs/mkfs.h"
#include "src/storage/blobfs/test/blob_utils.h"
#include "src/storage/blobfs/test/blobfs_test_setup.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"

namespace blobfs {

namespace {

constexpr uint32_t kTestDeviceBlockSize = 512;
constexpr uint32_t kTestDeviceNumBlocks = 400 * kBlobfsBlockSize / kTestDeviceBlockSize;
constexpr size_t kSmallBlobSize = 1024;
constexpr size_t kLargeBlobSize = kBlobfsBlockSize * 20;

// Large blobs must cover at least two levels in the Merkle tree to cover all branches.
static_assert(kLargeBlobSize > kBlobfsBlockSize);

struct DeliveryBlobTestParams {
  // Blob layout format that the Blobfs instance should be formatted with.
  BlobLayoutFormat format;

  // If true, specify that the delivery blob should be compressed.
  bool compress;

  // Size of the blob the test case should write.
  size_t blob_size;

  using ParamsAsTuple = std::tuple</*format*/ BlobLayoutFormat, /*compress*/ bool,
                                   /*blob_size*/ size_t>;

  explicit DeliveryBlobTestParams(ParamsAsTuple params)
      : format(std::get<0>(params)),
        compress(std::get<1>(params)),
        blob_size(std::get<2>(params)) {}

  static auto GetTestCombinations() {
    return testing::ConvertGenerator<ParamsAsTuple>(testing::Combine(
        /*format*/ testing::Values(BlobLayoutFormat::kCompactMerkleTreeAtEnd,
                                   BlobLayoutFormat::kDeprecatedPaddedMerkleTreeAtStart),
        /*compress*/ testing::Bool(),
        /*blob_size*/ testing::Values(0, kSmallBlobSize, kLargeBlobSize)));
  }

  static std::string GetTestParamName(const DeliveryBlobTestParams& params) {
    // These tests use rather large parameter names, so we use a more compact format when describing
    // which blob format the test case is using.
    std::string format_name;
    switch (params.format) {
      case blobfs::BlobLayoutFormat::kDeprecatedPaddedMerkleTreeAtStart:
        format_name = "DeprecatedFormat";
        break;
      case blobfs::BlobLayoutFormat::kCompactMerkleTreeAtEnd:
        format_name = "CompactFormat";
        break;
    }
    return format_name + std::string(params.compress ? "Compressed" : "Uncompressed") +
           std::string(params.blob_size > 0 ? std::to_string(params.blob_size) : "NullBlob");
  }
};

class DeliveryBlobTest : public BlobfsTestSetup,
                         public testing::TestWithParam<DeliveryBlobTestParams> {
 public:
  void SetUp() override {
    auto device =
        std::make_unique<block_client::FakeBlockDevice>(kTestDeviceNumBlocks, kTestDeviceBlockSize);

    const FilesystemOptions filesystem_options{
        .blob_layout_format = GetParam().format,
    };
    ASSERT_EQ(FormatFilesystem(device.get(), filesystem_options), ZX_OK);

    const MountOptions mount_options{
        .allow_delivery_blobs = true,
    };
    ASSERT_EQ(ZX_OK, Mount(std::move(device), mount_options));
    ASSERT_EQ(ZX_OK, blobfs()->OpenRootNode(&root_));
  }

  void TearDown() override {
    if (root_) {
      ASSERT_EQ(ZX_OK, root_->Close());
    }
  }

  const fbl::RefPtr<fs::Vnode>& root() const {
    ZX_ASSERT(root_);
    return root_;
  }

 private:
  fbl::RefPtr<fs::Vnode> root_ = nullptr;
};

TEST_P(DeliveryBlobTest, WriteAll) {
  const std::unique_ptr<BlobInfo> info =
      GenerateRandomBlob(/*mount_path*/ "", GetParam().blob_size);
  const fbl::Array<uint8_t> delivery_blob =
      GenerateDeliveryBlobType1({info->data.get(), info->size_data}, GetParam().compress).value();

  fbl::RefPtr<fs::Vnode> file;
  ASSERT_EQ(ZX_OK, root()->Create(GetDeliveryBlobPath(info->GetMerkleRoot()), 0, &file));
  ASSERT_EQ(ZX_OK, file->Truncate(delivery_blob.size()));
  size_t out_actual;
  ASSERT_EQ(ZX_OK, file->Write(delivery_blob.data(), delivery_blob.size(), 0, &out_actual));
  ASSERT_EQ(out_actual, delivery_blob.size());
  ASSERT_EQ(ZX_OK, file->Close());

  // Try to open the newly created blob.
  fbl::RefPtr<fs::Vnode> file_ptr;
  ASSERT_EQ(ZX_OK, root()->Lookup(info->GetMerkleRoot(), &file_ptr));
  ASSERT_EQ(ZX_OK, file_ptr->OpenValidating({}, &file));

  // Validate file contents.
  if (GetParam().blob_size > 0) {
    std::vector<uint8_t> file_contents(info->size_data);
    ASSERT_EQ(ZX_OK, file->Read(file_contents.data(), info->size_data, 0, &out_actual));
    ASSERT_EQ(out_actual, info->size_data);
    ASSERT_EQ(std::memcmp(info->data.get(), file_contents.data(), info->size_data), 0)
        << "Blob contents don't match after writing to disk.";
  }

  ASSERT_EQ(ZX_OK, file->Close());
}

TEST_P(DeliveryBlobTest, WriteChunked) {
  const std::unique_ptr<BlobInfo> info =
      GenerateRandomBlob(/*mount_path*/ "", GetParam().blob_size);
  const fbl::Array<uint8_t> delivery_blob =
      GenerateDeliveryBlobType1({info->data.get(), info->size_data}, GetParam().compress).value();

  fbl::RefPtr<fs::Vnode> file;
  ASSERT_EQ(root()->Create(GetDeliveryBlobPath(info->GetMerkleRoot()), 0, &file), ZX_OK);
  ASSERT_EQ(file->Truncate(delivery_blob.size()), ZX_OK);

  // Write the delivery blob in chunks. We use a very small chunk size to cover more edge cases.
  constexpr size_t kChunkSize = 4;
  size_t bytes_written = 0;
  while (bytes_written < delivery_blob.size()) {
    const size_t to_write = std::min(kChunkSize, delivery_blob.size() - bytes_written);
    size_t out_actual;
    ASSERT_EQ(ZX_OK, file->Write(delivery_blob.data() + bytes_written, to_write, bytes_written,
                                 &out_actual))
        << "Failed to write " << to_write << " bytes at offset " << bytes_written;
    ASSERT_EQ(out_actual, to_write);
    bytes_written += out_actual;
  }

  ASSERT_EQ(bytes_written, delivery_blob.size());
  ASSERT_EQ(file->Close(), ZX_OK);

  // Try to open the newly created blob.
  fbl::RefPtr<fs::Vnode> file_ptr;
  ASSERT_EQ(ZX_OK, root()->Lookup(info->GetMerkleRoot(), &file_ptr));
  ASSERT_EQ(ZX_OK, file_ptr->OpenValidating({}, &file));

  // Validate file contents.
  if (GetParam().blob_size > 0) {
    std::vector<uint8_t> file_contents(info->size_data);
    size_t out_actual;
    ASSERT_EQ(ZX_OK, file->Read(file_contents.data(), info->size_data, 0, &out_actual));
    ASSERT_EQ(out_actual, info->size_data);
    ASSERT_EQ(std::memcmp(info->data.get(), file_contents.data(), info->size_data), 0)
        << "Blob contents don't match after writing to disk.";
  }

  ASSERT_EQ(ZX_OK, file->Close());
}

// Verify that CalculateDeliveryBlobDigest works with all types of generated delivery blobs.
TEST_P(DeliveryBlobTest, CalculateDeliveryBlobDigest) {
  const std::unique_ptr<BlobInfo> info =
      GenerateRandomBlob(/*mount_path*/ "", GetParam().blob_size);
  const fbl::Array<uint8_t> delivery_blob =
      GenerateDeliveryBlobType1({info->data.get(), info->size_data}, GetParam().compress).value();
  zx::result<digest::Digest> digest = CalculateDeliveryBlobDigest(delivery_blob);
  ASSERT_TRUE(digest.is_ok()) << digest.status_string() << "Data length: " << delivery_blob.size();
  const auto digest_str = digest->ToString();
  ASSERT_EQ(info->GetMerkleRoot(), digest_str);
}

std::string GetTestParamName(const ::testing::TestParamInfo<DeliveryBlobTestParams>& p) {
  return DeliveryBlobTestParams::GetTestParamName(p.param);
}

INSTANTIATE_TEST_SUITE_P(
    /*no prefix*/, DeliveryBlobTest, DeliveryBlobTestParams::GetTestCombinations(),
    GetTestParamName);

}  // namespace
}  // namespace blobfs
