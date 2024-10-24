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

DeliveryBlobInfo GenerateBlob(uint64_t size) {
  auto buf = std::make_unique<uint8_t[]>(size);
  memset(buf.get(), 0xAB, size);
  // Don't compress the blob. The BlobCreator protocol doesn't need to test writing compressed blobs
  // and compressing blobs makes it harder to reason about the size of payloads.
  auto delivery_blob = GenerateDeliveryBlobType1(std::span(buf.get(), size), /*compress=*/false);
  ZX_ASSERT(delivery_blob.is_ok());
  auto digest = CalculateDeliveryBlobDigest(*delivery_blob);
  ZX_ASSERT(digest.is_ok());
  return DeliveryBlobInfo{
      .digest = std::move(*digest),
      .delivery_blob = std::move(*delivery_blob),
  };
}

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
    uint64_t payload_size = blob.delivery_blob.size();

    auto writer = Create(blob.digest);
    if (writer.is_error()) {
      return writer.take_error();
    }

    auto vmo = writer->GetVmo(payload_size);
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
      if (zx::result result = writer->BytesReady(bytes_to_write); result.is_error()) {
        return result.take_error();
      }
      bytes_written += bytes_to_write;
    }
    return zx::ok();
  }

 private:
  zx::result<BlobWriterWrapper> Create(const Digest& digest, bool allow_existing) const {
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
  auto blob = GenerateBlob(10);
  EXPECT_OK(creator().CreateAndWriteBlob(blob).status_value());
}

TEST_F(BlobCreatorTest, CreateExistingBlobFails) {
  auto blob = GenerateBlob(10);
  EXPECT_OK(creator().CreateAndWriteBlob(blob).status_value());

  auto writer = creator().Create(blob.digest);
  EXPECT_STATUS(writer.status_value(), ZX_ERR_ALREADY_EXISTS);
}

TEST_F(BlobCreatorTest, CreateWithAllowExistingIsNotSupported) {
  auto blob = GenerateBlob(10);
  auto writer = creator().CreateExisting(blob.digest);
  EXPECT_STATUS(writer.status_value(), ZX_ERR_INTERNAL);
}

using BlobWriterTest = BlobCreatorTest;

TEST_F(BlobWriterTest, ValidateRingBufferSize) {
  auto blob = GenerateBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(vmo.status_value());
  ASSERT_EQ(GetVmoSize(*vmo), kRingBufferSize);
}

TEST_F(BlobWriterTest, MultipleGetVmoCallsFail) {
  auto blob = GenerateBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(writer_vmo.status_value());

  EXPECT_STATUS(writer->GetVmo(blob.delivery_blob.size()).status_value(), ZX_ERR_BAD_STATE);
}

TEST_F(BlobWriterTest, GetVmoWithTooSmallOfPayloadFails) {
  // BlobWriter only accepts delivery blobs and GetVmo should validate that the payload is at least
  // the size of the delivery blob header.
  auto blob = GenerateBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(MetadataType1::kHeader.header_length - 1);
  EXPECT_STATUS(writer_vmo.status_value(), ZX_ERR_BAD_STATE);
}

TEST_F(BlobWriterTest, BytesReadyBeforeGetVmoFails) {
  auto blob = GenerateBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  ASSERT_STATUS(writer->BytesReady(5).status_value(), ZX_ERR_BAD_STATE);
}

TEST_F(BlobWriterTest, WritingMoreThanTheRingBufferFails) {
  auto blob = GenerateBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto writer_vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(writer_vmo.status_value());
  ASSERT_STATUS(writer->BytesReady(kRingBufferSize + 1).status_value(), ZX_ERR_OUT_OF_RANGE);
}

TEST_F(BlobWriterTest, WritingMoreBytesThanExpectedFails) {
  auto blob = GenerateBlob(10);
  auto writer = creator().Create(blob.digest);
  ASSERT_OK(writer.status_value());
  auto vmo = writer->GetVmo(blob.delivery_blob.size());
  ASSERT_OK(vmo.status_value());
  ASSERT_GT(GetVmoSize(*vmo), blob.delivery_blob.size());

  ASSERT_STATUS(writer->BytesReady(blob.delivery_blob.size() + 1).status_value(),
                ZX_ERR_BUFFER_TOO_SMALL);
}

TEST_F(BlobWriterTest, WrittenBlobIsReadable) {
  auto blob = GenerateBlob(10);
  ASSERT_OK(creator().CreateAndWriteBlob(blob).status_value());

  auto reader_vmo = reader().GetVmo(blob.digest);
  ASSERT_OK(reader_vmo.status_value());
}

TEST_F(BlobWriterTest, ZeroBytesReadyIsValid) {
  auto blob = GenerateBlob(10);
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
  auto blob = GenerateBlob(10);
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
  auto blob = GenerateBlob(10);
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
  auto blob = GenerateBlob(0);
  ASSERT_OK(creator().CreateAndWriteBlob(blob).status_value());

  auto reader_vmo = reader().GetVmo(blob.digest);
  ASSERT_OK(reader_vmo.status_value());
  EXPECT_EQ(GetVmoSize(*reader_vmo), 0lu);
}

TEST_F(BlobWriterTest, BytesReadySpanningEndOfVmo) {
  auto blob = GenerateBlob(kRingBufferSize + 50);
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
  auto blob = GenerateBlob(10);
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
  auto blob = GenerateBlob(10);
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

using BlobReaderTest = BlobCreatorTest;

TEST_F(BlobReaderTest, GetVmoForMissingBlobFails) {
  auto blob = GenerateBlob(10);
  auto vmo = reader().GetVmo(blob.digest);
  EXPECT_STATUS(vmo.status_value(), ZX_ERR_NOT_FOUND);
}

TEST_F(BlobReaderTest, GetVmoForPartiallyWrittenBlobFails) {
  auto blob = GenerateBlob(10);
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
