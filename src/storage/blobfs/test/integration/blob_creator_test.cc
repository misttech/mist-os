// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.fxfs/cpp/common_types.h>
#include <fidl/fuchsia.fxfs/cpp/markers.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fidl/cpp/wire/array.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <memory>
#include <span>
#include <utility>

#include <fbl/array.h>
#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"
#include "src/storage/blobfs/delivery_blob.h"
#include "src/storage/blobfs/delivery_blob_private.h"
#include "src/storage/blobfs/format.h"
#include "src/storage/blobfs/test/integration/blobfs_fixtures.h"

namespace blobfs {
namespace {

constexpr uint64_t kRingBufferSize = 256ul * 1024;

struct DeliveryBlobInfo {
  Digest digest;
  fbl::Array<uint8_t> delivery_blob;
};

DeliveryBlobInfo GenerateBlob(uint64_t size, bool compress) {
  auto buf = std::make_unique<uint8_t[]>(size);
  memset(buf.get(), 0xAB, size);
  // Don't compress the blob. The BlobCreator protocol doesn't need to test writing compressed blobs
  // and compressing blobs makes it harder to reason about the size of payloads.
  auto delivery_blob = GenerateDeliveryBlobType1(std::span(buf.get(), size), compress);
  ZX_ASSERT(delivery_blob.is_ok());
  auto digest = CalculateDeliveryBlobDigest(*delivery_blob);
  ZX_ASSERT(digest.is_ok());
  return DeliveryBlobInfo{
      .digest = std::move(*digest),
      .delivery_blob = std::move(*delivery_blob),
  };
}

DeliveryBlobInfo GenerateCompressedBlob(uint64_t size) { return GenerateBlob(size, true); }

DeliveryBlobInfo GenerateUncompressedBlob(uint64_t size) { return GenerateBlob(size, false); }

class BlobReaderWrapper {
 public:
  explicit BlobReaderWrapper(fidl::WireSyncClient<fuchsia_fxfs::BlobReader> reader)
      : reader_(std::move(reader)) {}

  zx::result<zx::vmo> GetVmo(const Digest& digest) const {
    fidl::Array<uint8_t, 32> hash;
    digest.CopyTo(hash.data_);
    auto result = reader_->GetVmo(hash);
    ZX_ASSERT(result.ok());
    if (result->is_error()) {
      return zx::error(result->error_value());
    }
    return zx::ok(std::move((*result)->vmo));
  }

 private:
  fidl::WireSyncClient<fuchsia_fxfs::BlobReader> reader_;
};

class BlobWriterWrapper {
 public:
  explicit BlobWriterWrapper(fidl::WireSyncClient<fuchsia_fxfs::BlobWriter> writer)
      : writer_(std::move(writer)) {}

  zx::result<> BytesReady(uint64_t bytes_written) const {
    auto result = writer_->BytesReady(bytes_written);
    ZX_ASSERT(result.ok());
    if (result->is_error()) {
      return zx::error(result->error_value());
    }
    return zx::ok();
  }

  zx::result<zx::vmo> GetVmo(uint64_t size) const {
    auto result = writer_->GetVmo(size);
    ZX_ASSERT(result.ok());
    if (result->is_error()) {
      return zx::error(result->error_value());
    }
    return zx::ok(std::move((*result)->vmo));
  }

  zx::result<> WriteBlob(const DeliveryBlobInfo& blob) const {
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

  fidl::WireSyncClient<fuchsia_fxfs::BlobWriter>& writer() { return writer_; }

 private:
  fidl::WireSyncClient<fuchsia_fxfs::BlobWriter> writer_;
};

class BlobCreatorWrapper {
 public:
  explicit BlobCreatorWrapper(fidl::WireSyncClient<fuchsia_fxfs::BlobCreator> creator)
      : creator_(std::move(creator)) {}

  zx::result<BlobWriterWrapper> Create(const Digest& digest) const { return Create(digest, false); }

  zx::result<BlobWriterWrapper> CreateExisting(const Digest& digest) const {
    return Create(digest, true);
  }

  zx::result<> CreateAndWriteBlob(const DeliveryBlobInfo& blob) const {
    auto writer = Create(blob.digest);
    if (writer.is_error()) {
      return writer.take_error();
    }

    return writer->WriteBlob(blob);
  }

 private:
  zx::result<BlobWriterWrapper> Create(const Digest& digest, bool allow_existing) const {
    fidl::Array<uint8_t, 32> hash;
    digest.CopyTo(hash.data_);
    auto result = creator_->Create(hash, allow_existing);
    ZX_ASSERT_MSG(result.ok(), "%s", result.status_string());
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

  fidl::WireSyncClient<fuchsia_fxfs::BlobCreator> creator_;
};

class BlobCreatorTest : public BlobfsTest {
 public:
  void SetUp() final {
    auto reader = component::ConnectAt<fuchsia_fxfs::BlobReader>(fs().ServiceDirectory());
    ZX_ASSERT(reader.is_ok());
    reader_ = std::make_unique<BlobReaderWrapper>(
        fidl::WireSyncClient<fuchsia_fxfs::BlobReader>(std::move(*reader)));

    auto creator = component::ConnectAt<fuchsia_fxfs::BlobCreator>(fs().ServiceDirectory());
    ZX_ASSERT(creator.is_ok());
    creator_ = std::make_unique<BlobCreatorWrapper>(
        fidl::WireSyncClient<fuchsia_fxfs::BlobCreator>(std::move(*creator)));
  }

  const BlobReaderWrapper& reader() const { return *reader_; }
  const BlobCreatorWrapper& creator() const { return *creator_; }
  void Barrier() const {
    // This is just a barrier to reduce the risk of a race. There is no way for the caller to wait
    // and guarantee that the vmo close has gotten to the port on the server. So we send some
    // other message that will be handled by the server to ensure that we do some kind of waiting on
    // it. Since that server is single-threaded it is unlikely that the close has not made it back
    // to start handling before it gets the next message after this one.
    auto info = fs().GetFsInfo();
    ASSERT_OK(info.status_value());
  }

 private:
  std::unique_ptr<BlobReaderWrapper> reader_;
  std::unique_ptr<BlobCreatorWrapper> creator_;
};

uint64_t GetVmoSize(const zx::vmo& vmo) {
  uint64_t vmo_size;
  ZX_ASSERT(vmo.get_size(&vmo_size) == ZX_OK);
  return vmo_size;
}

uint64_t GetVmoContentSize(const zx::vmo& vmo) {
  uint64_t vmo_content_size;
  ZX_ASSERT(vmo.get_prop_content_size(&vmo_content_size) == ZX_OK);
  return vmo_content_size;
}

TEST_F(BlobCreatorTest, CreateNewBlobSucceeds) {
  auto blob = GenerateUncompressedBlob(10);
  EXPECT_OK(creator().CreateAndWriteBlob(blob).status_value());
}

TEST_F(BlobCreatorTest, CreateExistingBlobFails) {
  auto blob = GenerateUncompressedBlob(10);
  EXPECT_OK(creator().CreateAndWriteBlob(blob).status_value());

  auto writer = creator().Create(blob.digest);
  EXPECT_STATUS(writer.status_value(), ZX_ERR_ALREADY_EXISTS);
}

TEST_F(BlobCreatorTest, TwoWritesToOneDigestFails) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());

  // Can't start two writers for the same digest at once.
  EXPECT_STATUS(creator().CreateExisting(blob.digest).status_value(), ZX_ERR_ALREADY_EXISTS);

  ASSERT_OK(writer->WriteBlob(blob).status_value());

  // Open the blob under the old version.
  auto vmo = reader().GetVmo(blob.digest);
  EXPECT_OK(vmo.status_value());

  // Other write completed. This can be replaced now. Note that the other connection isn't even
  // closed yet, but the blob has become readable.
  auto writer2 = creator().CreateExisting(blob.digest);
  EXPECT_OK(writer2.status_value());

  // Can't overwrite while the other overwrite is in progress.
  EXPECT_STATUS(creator().CreateExisting(blob.digest).status_value(), ZX_ERR_ALREADY_EXISTS);
  ASSERT_OK(writer2->WriteBlob(blob).status_value());

  // Try to read with the new info.
  char a;
  EXPECT_OK(vmo->read(&a, 0, 1));
}

TEST_F(BlobCreatorTest, AbandonOverwriteAllowsRestart) {
  auto blob = GenerateUncompressedBlob(10);
  ASSERT_OK(creator().CreateAndWriteBlob(blob).status_value());

  {
    auto writer = creator().CreateExisting(blob.digest);
    EXPECT_OK(writer.status_value());

    // Can't overwrite while the other overwrite is in progress.
    EXPECT_STATUS(creator().CreateExisting(blob.digest).status_value(), ZX_ERR_ALREADY_EXISTS);
  }

  // First writer went away, so now we can try again.
  auto writer = creator().CreateExisting(blob.digest);
  EXPECT_OK(writer.status_value());
  EXPECT_OK(writer->WriteBlob(blob).status_value());
}

TEST_F(BlobCreatorTest, FailOverwriteForUnlinked) {
  auto blob = GenerateUncompressedBlob(10);
  ASSERT_OK(creator().CreateAndWriteBlob(blob).status_value());
  auto fd = fs().GetRootFd();
  {
    // Grab the vmo to survive unlink.
    auto vmo = reader().GetVmo(blob.digest);
    EXPECT_OK(vmo.status_value());

    // Unlink.
    ASSERT_EQ(unlinkat(fd.get(), blob.digest.ToString().c_str(), 0), 0);

    EXPECT_STATUS(creator().CreateExisting(blob.digest).status_value(), ZX_ERR_ALREADY_EXISTS);
  }
}

TEST_F(BlobCreatorTest, UnlinkPreventedByOverwrite) {
  uint64_t old_used_bytes;
  {
    auto info = fs().GetFsInfo();
    ASSERT_OK(info.status_value());
    old_used_bytes = info->used_bytes;
  }

  auto blob = GenerateUncompressedBlob(10);
  ASSERT_OK(creator().CreateAndWriteBlob(blob).status_value());

  // Should have grown.
  {
    auto info = fs().GetFsInfo();
    ASSERT_OK(info.status_value());
    ASSERT_GT(info->used_bytes, old_used_bytes);
  }

  auto fd = fs().GetRootFd();
  {
    // Start overwrite.
    auto writer = creator().CreateExisting(blob.digest);
    EXPECT_OK(writer.status_value());

    // Unlink.
    ASSERT_EQ(unlinkat(fd.get(), blob.digest.ToString().c_str(), 0), 0);

    // Finish overwrite. This will let the purge complete.
    ASSERT_OK(writer.status_value());
  }

  // Should be back where we started.
  {
    auto info = fs().GetFsInfo();
    ASSERT_OK(info.status_value());
    ASSERT_EQ(info->used_bytes, old_used_bytes);
  }
}

TEST_F(BlobCreatorTest, UnlinkDuringOverwrite) {
  uint64_t old_used_bytes;
  {
    auto info = fs().GetFsInfo();
    ASSERT_OK(info.status_value());
    old_used_bytes = info->used_bytes;
  }

  auto blob = GenerateUncompressedBlob(10);
  ASSERT_OK(creator().CreateAndWriteBlob(blob).status_value());

  // Should have grown.
  {
    auto info = fs().GetFsInfo();
    ASSERT_OK(info.status_value());
    ASSERT_GT(info->used_bytes, old_used_bytes);
  }

  auto fd = fs().GetRootFd();
  {
    auto vmo = reader().GetVmo(blob.digest);
    EXPECT_OK(vmo.status_value());

    // Start overwrite.
    auto writer = creator().CreateExisting(blob.digest);
    EXPECT_OK(writer.status_value());

    // Unlink.
    ASSERT_EQ(unlinkat(fd.get(), blob.digest.ToString().c_str(), 0), 0);

    // Finish overwrite.
    ASSERT_OK(writer->WriteBlob(blob).status_value());

    // Try to read with the new info.
    char a;
    EXPECT_OK(vmo->read(&a, 0, 1));
  }

  Barrier();

  // Should be back where we started.
  {
    auto info = fs().GetFsInfo();
    ASSERT_OK(info.status_value());
    ASSERT_EQ(info->used_bytes, old_used_bytes);
  }
}

TEST_F(BlobCreatorTest, AllowExistingNullBlob) {
  auto blob = GenerateUncompressedBlob(0);
  EXPECT_OK(creator().CreateAndWriteBlob(blob).status_value());

  auto writer = creator().CreateExisting(blob.digest);
  EXPECT_OK(writer.status_value());
  EXPECT_OK(writer->WriteBlob(blob).status_value());

  EXPECT_OK(reader().GetVmo(blob.digest).status_value());
}

using BlobWriterTest = BlobCreatorTest;

TEST_F(BlobWriterTest, ValidateRingBufferSize) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(vmo.status_value());
  ASSERT_EQ(GetVmoSize(*vmo), kRingBufferSize);
}

TEST_F(BlobWriterTest, MultipleGetVmoCallsFail) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(writer_vmo.status_value());

  EXPECT_STATUS(writer->GetVmo(blob.delivery_blob.size()).status_value(), ZX_ERR_BAD_STATE);
}

TEST_F(BlobWriterTest, GetVmoWithTooSmallOfPayloadFails) {
  // BlobWriter only accepts delivery blobs and GetVmo should validate that the payload is at least
  // the size of the delivery blob header.
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(MetadataType1::kHeader.header_length - 1);
  EXPECT_STATUS(writer_vmo.status_value(), ZX_ERR_BAD_STATE);
}

TEST_F(BlobWriterTest, BytesReadyBeforeGetVmoFails) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  ASSERT_STATUS(writer->BytesReady(5).status_value(), ZX_ERR_BAD_STATE);
}

TEST_F(BlobWriterTest, WritingMoreThanTheRingBufferFails) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(writer_vmo.status_value());
  ASSERT_STATUS(writer->BytesReady(kRingBufferSize + 1).status_value(), ZX_ERR_OUT_OF_RANGE);
}

TEST_F(BlobWriterTest, WritingMoreBytesThanExpectedFails) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(vmo.status_value());
  ASSERT_GT(GetVmoSize(*vmo), blob.delivery_blob.size());

  ASSERT_STATUS(writer->BytesReady(blob.delivery_blob.size() + 1).status_value(),
                ZX_ERR_BUFFER_TOO_SMALL);
}

TEST_F(BlobWriterTest, WrittenBlobIsReadable) {
  auto blob = GenerateUncompressedBlob(10);
  ASSERT_OK(creator().CreateAndWriteBlob(blob).status_value());

  auto reader_vmo = reader().GetVmo(blob.digest);
  ASSERT_OK(reader_vmo.status_value());
}

TEST_F(BlobWriterTest, ZeroBytesReadyIsValid) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(writer_vmo.status_value());
  uint64_t vmo_size = GetVmoSize(*writer_vmo);
  ASSERT_GT(vmo_size, blob.delivery_blob.size());
  ASSERT_GT(blob.delivery_blob.size(), 20lu);

  ASSERT_OK(writer->BytesReady(0).status_value());
  ASSERT_OK(writer_vmo->write(blob.delivery_blob.get(), 0, blob.delivery_blob.size()));
  ASSERT_OK(writer->BytesReady(0).status_value());
  ASSERT_OK(writer->BytesReady(blob.delivery_blob.size()).status_value());
  ASSERT_OK(writer->BytesReady(0).status_value());

  auto reader_vmo = reader().GetVmo(blob.digest);
  ASSERT_OK(reader_vmo.status_value());
}

TEST_F(BlobWriterTest, MultipleWrites) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(writer_vmo.status_value());
  uint64_t vmo_size = GetVmoSize(*writer_vmo);
  ASSERT_GT(vmo_size, blob.delivery_blob.size());
  ASSERT_GT(blob.delivery_blob.size(), 20lu);
  ASSERT_OK(writer_vmo->write(blob.delivery_blob.get(), 0, blob.delivery_blob.size()));
  ASSERT_OK(writer->BytesReady(10).status_value());
  ASSERT_OK(writer->BytesReady(10).status_value());
  ASSERT_OK(writer->BytesReady(blob.delivery_blob.size() - 20).status_value());

  auto reader_vmo = reader().GetVmo(blob.digest);
  ASSERT_OK(reader_vmo.status_value());
}

TEST_F(BlobWriterTest, BytesReadyAfterBlobWrittenFails) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(writer_vmo.status_value());
  ASSERT_GT(GetVmoSize(*writer_vmo), blob.delivery_blob.size());
  ASSERT_OK(writer_vmo->write(blob.delivery_blob.get(), 0, blob.delivery_blob.size()));
  ASSERT_OK(writer->BytesReady(blob.delivery_blob.size()).status_value());

  // Continuing to write fails.
  ASSERT_STATUS(writer->BytesReady(1).status_value(), ZX_ERR_BUFFER_TOO_SMALL);

  // The blob was still written.
  auto reader_vmo = reader().GetVmo(blob.digest);
  ASSERT_OK(reader_vmo.status_value());
}

TEST_F(BlobWriterTest, WriteNullBlob) {
  auto blob = GenerateUncompressedBlob(0);
  ASSERT_OK(creator().CreateAndWriteBlob(blob).status_value());

  auto reader_vmo = reader().GetVmo(blob.digest);
  ASSERT_OK(reader_vmo.status_value());
  EXPECT_EQ(GetVmoSize(*reader_vmo), 0lu);
}

TEST_F(BlobWriterTest, BytesReadySpanningEndOfVmo) {
  auto blob = GenerateUncompressedBlob(kRingBufferSize + 50);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(writer_vmo.status_value());
  uint64_t vmo_size = GetVmoSize(*writer_vmo);
  ASSERT_EQ(vmo_size, kRingBufferSize);
  ASSERT_LT(vmo_size, blob.delivery_blob.size());
  ASSERT_GT(vmo_size * 2, blob.delivery_blob.size());

  // Write out the number of bytes that will wrap around the end of the VMO first. This allows up to
  // write the entire ring buffer second.
  uint64_t wrapped_byte_count = blob.delivery_blob.size() - kRingBufferSize;
  ASSERT_OK(writer_vmo->write(blob.delivery_blob.get(), 0, wrapped_byte_count));
  ASSERT_OK(writer->BytesReady(wrapped_byte_count));

  ASSERT_OK(writer_vmo->write(blob.delivery_blob.get() + wrapped_byte_count, wrapped_byte_count,
                              kRingBufferSize - wrapped_byte_count));
  ASSERT_OK(writer_vmo->write(blob.delivery_blob.get() + kRingBufferSize, 0, wrapped_byte_count));
  ASSERT_OK(writer->BytesReady(kRingBufferSize));

  auto reader_vmo = reader().GetVmo(blob.digest);
  ASSERT_OK(reader_vmo.status_value());
  EXPECT_EQ(GetVmoContentSize(*reader_vmo), kRingBufferSize + 50);
}

TEST_F(BlobWriterTest, CorruptBlobFails) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(writer_vmo.status_value());

  ASSERT_NE(blob.delivery_blob[blob.delivery_blob.size() - 1], 0xCD);
  blob.delivery_blob[blob.delivery_blob.size() - 1] = 0xCD;
  ASSERT_OK(writer_vmo->write(blob.delivery_blob.get(), 0, blob.delivery_blob.size()));
  ASSERT_STATUS(writer->BytesReady(blob.delivery_blob.size()).status_value(),
                ZX_ERR_IO_DATA_INTEGRITY);
}

TEST_F(BlobWriterTest, CorruptDeliveryBlobHeaderFails) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(writer_vmo.status_value());

  ASSERT_NE(blob.delivery_blob[0], 0xCD);
  blob.delivery_blob[0] = 0xCD;
  ASSERT_OK(writer_vmo->write(blob.delivery_blob.get(), 0, blob.delivery_blob.size()));
  ASSERT_STATUS(writer->BytesReady(blob.delivery_blob.size()).status_value(),
                ZX_ERR_IO_DATA_INTEGRITY);
}

TEST_F(BlobWriterTest, CreateWithAllowExistingDeletesOld) {
  constexpr uint64_t kBlobSize = 9000;  // Big enough for compression to make a difference.
  auto blob = GenerateUncompressedBlob(kBlobSize);
  ASSERT_STATUS(reader().GetVmo(blob.digest).status_value(), ZX_ERR_NOT_FOUND);

  {
    // Doing CreateExisting despite the blob not being there. Succeeds normally.
    auto writer = creator().CreateExisting(blob.digest);
    EXPECT_OK(writer.status_value());
    ASSERT_OK(writer->WriteBlob(blob).status_value());
  }

  // Get the current space usage.
  uint64_t old_used_bytes;
  {
    auto info = fs().GetFsInfo();
    ASSERT_OK(info.status_value());
    old_used_bytes = info->used_bytes;
    EXPECT_NE(old_used_bytes, 0ul);
  }

  auto compressed_blob = GenerateCompressedBlob(kBlobSize);
  {
    auto writer = creator().CreateExisting(blob.digest);
    EXPECT_OK(writer.status_value());
    EXPECT_OK(writer->WriteBlob(compressed_blob).status_value());
  }

  // Check that it can be verified.
  auto vmo = reader().GetVmo(blob.digest);
  EXPECT_OK(vmo.status_value());
  char a;
  EXPECT_OK(vmo->read(&a, 0, 1));

  // Check that space usage has been recovered by replacing with a compressed blob.
  // In part to verify that the old blob has actually been deleted.
  auto info = fs().GetFsInfo();
  ASSERT_OK(info.status_value());
  EXPECT_LT(info->used_bytes, old_used_bytes);
}

TEST_F(BlobWriterTest, FailedWriteMarksBlobCorruptRecoversOnRemount) {
  auto blob = GenerateUncompressedBlob(10);
  EXPECT_OK(creator().CreateAndWriteBlob(blob).status_value());
  auto fd = fs().GetRootFd();
  // Sync to ensure that the data makes it disk.
  ASSERT_EQ(fsync(fd.get()), 0);

  {
    auto vmo = reader().GetVmo(blob.digest);
    ASSERT_OK(vmo.status_value());

    auto writer = creator().CreateExisting(blob.digest);

    // Sleep the disk during the write to generate a failure.
    ASSERT_OK(fs().GetRamDisk()->SleepAfter(0).status_value());
    EXPECT_TRUE(writer->WriteBlob(blob).is_error());
    ASSERT_OK(fs().GetRamDisk()->Wake().status_value());

    // The original blob is partially deleted. It cannot respond.
    char a;
    EXPECT_STATUS(vmo->read(&a, 0, 1), ZX_ERR_BAD_STATE);
  }

  // Remount to revert changes to the ondisk version.
  ASSERT_OK(fs().Unmount().status_value());
  ASSERT_OK(fs().Mount().status_value());

  auto reader_chan = component::ConnectAt<fuchsia_fxfs::BlobReader>(fs().ServiceDirectory());
  ASSERT_OK(reader_chan.status_value());
  auto reader = std::make_unique<BlobReaderWrapper>(
      fidl::WireSyncClient<fuchsia_fxfs::BlobReader>(std::move(*reader_chan)));
  auto vmo = reader->GetVmo(blob.digest);
  ASSERT_OK(vmo.status_value());

  // Now reading works.
  char a;
  EXPECT_OK(vmo->read(&a, 0, 1));
}

TEST_F(BlobWriterTest, FailedOverwriteWithBadData) {
  auto blob = GenerateUncompressedBlob(10);
  EXPECT_OK(creator().CreateAndWriteBlob(blob).status_value());

  auto writer = creator().CreateExisting(blob.digest);
  ASSERT_OK(writer.status_value());

  auto vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(vmo.status_value());

  uint64_t payload_size = blob.delivery_blob.size() - 1;
  uint64_t bytes_written = 0;
  while (bytes_written < payload_size) {
    uint64_t bytes_to_write = std::min(kRingBufferSize, payload_size - bytes_written);
    ASSERT_OK(vmo->write(blob.delivery_blob.get() + bytes_written, 0, bytes_to_write));
    ASSERT_OK(writer->BytesReady(bytes_to_write).status_value());
    bytes_written += bytes_to_write;
  }
  // Write a bad byte at the end of the delivery blob.
  uint8_t bad_byte = blob.delivery_blob.data()[payload_size - 1] ^ 0xFF;
  ASSERT_OK(vmo->write(&bad_byte, 0, 1));
  EXPECT_STATUS(writer->BytesReady(1).status_value(), ZX_ERR_IO_DATA_INTEGRITY);
}

// Ensure that dropping a handle and reopening a blob do not race.
TEST_F(BlobWriterTest, CloseRaceTest) {
  auto blob = GenerateUncompressedBlob(10);
  for (int i = 0; i < 1000; ++i) {
    auto writer_or = creator().Create(blob.digest);
    ASSERT_OK(writer_or);
    auto writer = std::move(writer_or.value());
    ASSERT_OK(writer.GetVmo(blob.delivery_blob.size()).status_value());
  }
}

TEST_F(BlobWriterTest, OverwriteCloseRaceTest) {
  auto blob = GenerateUncompressedBlob(10);
  ASSERT_OK(creator().CreateAndWriteBlob(blob).status_value());
  for (int i = 0; i < 1000; ++i) {
    auto writer_or = creator().CreateExisting(blob.digest);
    ASSERT_OK(writer_or);
    auto writer = std::move(writer_or.value());
    ASSERT_OK(writer.GetVmo(blob.delivery_blob.size()).status_value());
  }
}

using BlobReaderTest = BlobCreatorTest;

TEST_F(BlobReaderTest, GetVmoForMissingBlobFails) {
  auto blob = GenerateUncompressedBlob(10);
  auto vmo = reader().GetVmo(blob.digest);
  EXPECT_STATUS(vmo.status_value(), ZX_ERR_NOT_FOUND);
}

TEST_F(BlobReaderTest, GetVmoForPartiallyWrittenBlobFails) {
  auto blob = GenerateUncompressedBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(writer_vmo.status_value());
  ASSERT_OK(writer_vmo->write(blob.delivery_blob.get(), 0, blob.delivery_blob.size() - 1));
  ASSERT_OK(writer->BytesReady(blob.delivery_blob.size() - 1).status_value());

  auto vmo = reader().GetVmo(blob.digest);
  EXPECT_STATUS(vmo.status_value(), ZX_ERR_NOT_FOUND);
}

}  // namespace
}  // namespace blobfs
