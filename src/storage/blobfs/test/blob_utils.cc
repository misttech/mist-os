// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/test/blob_utils.h"

#include <fcntl.h>
#include <fidl/fuchsia.fxfs/cpp/common_types.h>
#include <fidl/fuchsia.fxfs/cpp/markers.h>
#include <fidl/fuchsia.fxfs/cpp/wire_messaging.h>
#include <lib/fidl/cpp/wire/array.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <algorithm>
#include <cerrno>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <limits>
#include <memory>
#include <span>
#include <string>
#include <utility>
#include <vector>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <safemath/safe_conversions.h>
#include <zstd/zstd.h>

#include "src/lib/digest/digest.h"
#include "src/lib/digest/merkle-tree.h"
#include "src/storage/blobfs/blob_layout.h"
#include "src/storage/blobfs/delivery_blob.h"
#include "src/storage/blobfs/format.h"

namespace blobfs {
namespace {

using digest::MerkleTreeCreator;

std::vector<uint8_t> LoadTemplateData() {
  constexpr char kDataFile[] = "/pkg/data/test_binary.zstd";
  fbl::unique_fd fd(open(kDataFile, O_RDONLY));
  EXPECT_TRUE(fd.is_valid());
  if (!fd) {
    fprintf(stderr, "blob_utils.cc: Failed to load template data file %s: %s\n", kDataFile,
            strerror(errno));
    return {};
  }
  struct stat s;
  EXPECT_EQ(fstat(fd.get(), &s), 0);
  size_t sz = s.st_size;

  std::vector<uint8_t> compressed(sz);
  EXPECT_EQ(StreamAll(read, fd.get(), compressed.data(), sz), 0);

  constexpr size_t kUncompressedReserveSize{static_cast<size_t>(128) * 1024};
  std::vector<uint8_t> uncompressed(kUncompressedReserveSize);
  uncompressed.resize(ZSTD_decompress(uncompressed.data(), uncompressed.size(), compressed.data(),
                                      compressed.size()));
  return uncompressed;
}

}  // namespace

void RandomFill(uint8_t* data, size_t length) {
  for (size_t i = 0; i < length; i++) {
    // TODO(jfsulliv): Use explicit seed
    data[i] = static_cast<uint8_t>(rand());
  }
}

// Creates, writes, reads (to verify) and operates on a blob.
std::unique_ptr<BlobInfo> GenerateBlob(const BlobSrcFunction& data_generator,
                                       const std::string& mount_path, size_t data_size) {
  std::unique_ptr<BlobInfo> info(new BlobInfo);
  info->data = nullptr;
  info->size_data = data_size;
  if (data_size > 0) {
    info->data.reset(new uint8_t[data_size]);
    data_generator(info->data.get(), data_size);
  }

  auto merkle_tree = CreateMerkleTree(info->data.get(), data_size, /*use_compact_format=*/true);
  // Ensure we include a path separator if mount_path is specified and does not include one.
  const bool requires_separator = !mount_path.empty() && *mount_path.cend() != '/';
  snprintf(info->path, sizeof(info->path), "%s%s%s", mount_path.c_str(),
           requires_separator ? "/" : "", merkle_tree->root.ToString().c_str());

  return info;
}

std::unique_ptr<BlobInfo> GenerateRandomBlob(const std::string& mount_path, size_t data_size) {
  return GenerateBlob(RandomFill, mount_path, data_size);
}

std::unique_ptr<BlobInfo> GenerateRealisticBlob(const std::string& mount_path, size_t data_size) {
  static auto* template_data = [] {
    auto* data = new std::vector(LoadTemplateData());
    ZX_ASSERT_MSG(data->size() > 0ul, "Failed to load realistic data");
    return data;
  }();
  return GenerateBlob(
      [](uint8_t* data, size_t length) {
        // TODO(jfsulliv): Use explicit seed
        int nonce = rand();
        size_t nonce_size = std::min(sizeof(nonce), length);
        memcpy(data, &nonce, nonce_size);
        data += nonce_size;
        length -= nonce_size;

        while (length > 0) {
          size_t to_copy = std::min(template_data->size(), length);
          memcpy(data, template_data->data(), to_copy);
          data += to_copy;
          length -= to_copy;
        }
      },
      mount_path, data_size);
}

void VerifyContents(int fd, const uint8_t* data, size_t data_size) {
  ASSERT_EQ(0, lseek(fd, 0, SEEK_SET));

  // Cast |data_size| to ssize_t to match the return type of |read| and avoid narrowing conversion
  // warnings from mixing size_t and ssize_t.
  ZX_ASSERT(std::numeric_limits<ssize_t>::max() >= data_size);
  ssize_t data_size_signed = safemath::checked_cast<ssize_t>(data_size);

  constexpr ssize_t kBuffersize = 8192;
  std::unique_ptr<char[]> buffer(new char[kBuffersize]);

  for (ssize_t total_read = 0; total_read < data_size_signed; total_read += kBuffersize) {
    ssize_t read_size = std::min(kBuffersize, data_size_signed - total_read);
    ASSERT_EQ(read_size, read(fd, buffer.get(), read_size));
    ASSERT_EQ(memcmp(&data[total_read], buffer.get(), read_size), 0);
  }
}

void MakeBlob(const BlobInfo& info, fbl::unique_fd* fd) {
  fd->reset(open(info.path, O_CREAT | O_RDWR, S_IRUSR | S_IWUSR));
  ASSERT_TRUE(*fd) << "Open failed: " << strerror(errno);
  ASSERT_EQ(ftruncate(fd->get(), info.size_data), 0);
  ASSERT_EQ(StreamAll(write, fd->get(), info.data.get(), info.size_data), 0);
  VerifyContents(fd->get(), info.data.get(), info.size_data);
}

std::string GetBlobLayoutFormatNameForTests(BlobLayoutFormat format) {
  switch (format) {
    case BlobLayoutFormat::kDeprecatedPaddedMerkleTreeAtStart:
      return "PaddedMerkleTreeAtStartLayout";
    case BlobLayoutFormat::kCompactMerkleTreeAtEnd:
      return "CompactMerkleTreeAtEndLayout";
  }
}

std::unique_ptr<MerkleTreeInfo> CreateMerkleTree(const uint8_t* data, uint64_t data_size,
                                                 bool use_compact_format) {
  auto merkle_tree_info = std::make_unique<MerkleTreeInfo>();
  MerkleTreeCreator mtc;
  mtc.SetUseCompactFormat(use_compact_format);
  zx_status_t status = mtc.SetDataLength(data_size);
  ZX_ASSERT_MSG(status == ZX_OK, "Failed to set data length: %s", zx_status_get_string(status));
  merkle_tree_info->merkle_tree_size = mtc.GetTreeLength();
  if (merkle_tree_info->merkle_tree_size > 0) {
    merkle_tree_info->merkle_tree.reset(new uint8_t[merkle_tree_info->merkle_tree_size]);
  }
  uint8_t merkle_tree_root[digest::kSha256Length];
  status = mtc.SetTree(merkle_tree_info->merkle_tree.get(), merkle_tree_info->merkle_tree_size,
                       merkle_tree_root, digest::kSha256Length);
  ZX_ASSERT_MSG(status == ZX_OK, "Failed to set Merkle tree: %s", zx_status_get_string(status));
  status = mtc.Append(data, data_size);
  ZX_ASSERT_MSG(status == ZX_OK, "Failed to add data to Merkle tree: %s",
                zx_status_get_string(status));
  merkle_tree_info->root = merkle_tree_root;
  return merkle_tree_info;
}

std::string BlobInfo::GetMerkleRoot() const { return std::filesystem::path(path).filename(); }

TestDeliveryBlob TestDeliveryBlob::CreateCompressed(uint64_t size) {
  auto buf = std::make_unique<uint8_t[]>(size);
  memset(buf.get(), 0xAB, size);
  return CreateCompressed(std::span(buf.get(), size));
}

TestDeliveryBlob TestDeliveryBlob::CreateCompressed(std::span<uint8_t> blob_data) {
  auto delivery_blob = GenerateDeliveryBlobType1(blob_data, /*compress=*/true);
  ZX_ASSERT(delivery_blob.is_ok());
  auto digest = CalculateDeliveryBlobDigest(*delivery_blob);
  ZX_ASSERT(digest.is_ok());
  return TestDeliveryBlob{
      .digest = std::move(*digest),
      .delivery_blob = std::move(*delivery_blob),
  };
}

TestDeliveryBlob TestDeliveryBlob::CreateUncompressed(uint64_t size) {
  auto buf = std::make_unique<uint8_t[]>(size);
  memset(buf.get(), 0xAB, size);
  return CreateUncompressed(std::span(buf.get(), size));
}

TestDeliveryBlob TestDeliveryBlob::CreateUncompressed(std::span<uint8_t> blob_data) {
  auto delivery_blob = GenerateDeliveryBlobType1(blob_data, /*compress=*/false);
  ZX_ASSERT(delivery_blob.is_ok());
  auto digest = CalculateDeliveryBlobDigest(*delivery_blob);
  ZX_ASSERT(digest.is_ok());
  return TestDeliveryBlob{
      .digest = std::move(*digest),
      .delivery_blob = std::move(*delivery_blob),
  };
}

BlobReaderWrapper::BlobReaderWrapper(fidl::WireSyncClient<fuchsia_fxfs::BlobReader> reader)
    : reader_(std::move(reader)) {}

zx::result<zx::vmo> BlobReaderWrapper::GetVmo(const Digest& digest) const {
  fidl::Array<uint8_t, 32> hash;
  digest.CopyTo(hash.data_);
  auto result = reader_->GetVmo(hash);
  ZX_ASSERT(result.ok());
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  return zx::ok(std::move((*result)->vmo));
}

BlobWriterWrapper::BlobWriterWrapper(fidl::WireSyncClient<fuchsia_fxfs::BlobWriter> writer)
    : writer_(std::move(writer)) {}

zx::result<> BlobWriterWrapper::BytesReady(uint64_t bytes_written) {
  auto result = writer_->BytesReady(bytes_written);
  ZX_ASSERT(result.ok());
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  return zx::ok();
}

zx::result<zx::vmo> BlobWriterWrapper::GetVmo(uint64_t size) {
  auto result = writer_->GetVmo(size);
  ZX_ASSERT(result.ok());
  if (result->is_error()) {
    return zx::error(result->error_value());
  }
  return zx::ok(std::move((*result)->vmo));
}

zx::result<> BlobWriterWrapper::WriteBlob(const TestDeliveryBlob& blob) {
  uint64_t payload_size = blob.delivery_blob.size();
  auto vmo = GetVmo(payload_size);
  if (vmo.is_error()) {
    return vmo.take_error();
  }

  uint64_t bytes_written = 0;
  while (bytes_written < payload_size) {
    uint64_t bytes_to_write = std::min(kRingBufferSize, payload_size - bytes_written);
    if (zx_status_t status =
            vmo->write(blob.delivery_blob.get() + bytes_written, 0, bytes_to_write);
        status != ZX_OK) {
      return zx::error(status);
    }
    if (zx::result result = BytesReady(bytes_to_write); result.is_error()) {
      return result.take_error();
    }
    bytes_written += bytes_to_write;
  }
  return zx::ok();
}

BlobCreatorWrapper::BlobCreatorWrapper(fidl::WireSyncClient<fuchsia_fxfs::BlobCreator> creator)
    : creator_(std::move(creator)) {}

zx::result<BlobWriterWrapper> BlobCreatorWrapper::Create(const Digest& digest) const {
  return Create(digest, false);
}

zx::result<BlobWriterWrapper> BlobCreatorWrapper::CreateExisting(const Digest& digest) const {
  return Create(digest, true);
}

zx::result<> BlobCreatorWrapper::CreateAndWriteBlob(const TestDeliveryBlob& blob) const {
  auto writer = Create(blob.digest);
  if (writer.is_error()) {
    return writer.take_error();
  }

  return writer->WriteBlob(blob);
}

zx::result<BlobWriterWrapper> BlobCreatorWrapper::Create(const Digest& digest,
                                                         bool allow_existing) const {
  fidl::Array<uint8_t, 32> hash;
  digest.CopyTo(hash.data_);
  auto result = creator_->Create(hash, allow_existing);
  ZX_ASSERT(result.ok());
  if (result->is_error()) {
    switch (result->error_value()) {
      case fuchsia_fxfs::CreateBlobError::kAlreadyExists:
        return zx::error(ZX_ERR_ALREADY_EXISTS);
      case fuchsia_fxfs::CreateBlobError::kInternal:
        return zx::error(ZX_ERR_INTERNAL);
    }
  }
  return zx::ok(BlobWriterWrapper(
      fidl::WireSyncClient<fuchsia_fxfs::BlobWriter>(std::move((*result)->writer))));
}

}  // namespace blobfs
