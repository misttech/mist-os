// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/blob_creator.h"

#include <fidl/fuchsia.fxfs/cpp/wire.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/status.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <optional>
#include <utility>

#include <fbl/ref_ptr.h>

#include "src/storage/blobfs/blob.h"
#include "src/storage/blobfs/blobfs.h"
#include "src/storage/blobfs/delivery_blob_private.h"
#include "src/storage/blobfs/format.h"
#include "src/storage/lib/vfs/cpp/service.h"

namespace blobfs {
namespace {

constexpr uint64_t kRingBufferSize = 256ul * 1024;

class BlobWriter final : public fidl::WireServer<fuchsia_fxfs::BlobWriter> {
 public:
  explicit BlobWriter(fbl::RefPtr<Blob> blob) : blob_(std::move(blob)) {}

  void GetVmo(fuchsia_fxfs::wire::BlobWriterGetVmoRequest* request,
              GetVmoCompleter::Sync& completer) final {
    auto result = GetVmoImpl(request->size);
    if (result.is_error()) {
      completer.ReplyError(result.status_value());
    } else {
      completer.ReplySuccess(std::move(result).value());
    }
  }

  void BytesReady(fuchsia_fxfs::wire::BlobWriterBytesReadyRequest* request,
                  BytesReadyCompleter::Sync& completer) final {
    if (auto result = BytesReadyImpl(request->bytes_written); result.is_error()) {
      completer.ReplyError(result.status_value());
      completer.Close(result.status_value());
    } else {
      completer.ReplySuccess();
    }
  }

 private:
  zx::result<zx::vmo> GetVmoImpl(uint64_t size) {
    if (expected_size_.has_value() || vmo_.is_valid()) {
      // GetVmo was already called.
      return zx::error(ZX_ERR_BAD_STATE);
    }
    if (size < MetadataType1::kHeader.header_length) {
      // The number of bytes being transferred is less than the size of the delivery blob header.
      return zx::error(ZX_ERR_BAD_STATE);
    }
    expected_size_ = size;
    if (zx_status_t status = blob_->Truncate(size); status != ZX_OK) {
      return zx::error(status);
    }
    zx::vmo vmo;
    if (zx_status_t status = zx::vmo::create(kRingBufferSize, 0, &vmo); status != ZX_OK) {
      return zx::error(status);
    }
    zx::vmo vmo_dup;
    if (zx_status_t status = vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo_dup); status != ZX_OK) {
      return zx::error(status);
    }
    if (zx_status_t status = vmo_mapper_.Map(vmo); status != ZX_OK) {
      return zx::error(status);
    }
    buffer_ = static_cast<uint8_t*>(vmo_mapper_.start());
    vmo_ = std::move(vmo);
    return zx::ok(std::move(vmo_dup));
  }

  zx::result<> WriteBufferToBlob(uint64_t buffer_offset, uint64_t len) {
    size_t actual = 0;
    if (zx_status_t status = blob_->Write(buffer_ + buffer_offset, len, total_written_, &actual);
        status != ZX_OK) {
      return zx::error(status);
    }
    // BlobWriter doesn't support partial writes.
    ZX_DEBUG_ASSERT(actual == len);
    total_written_ += len;
    return zx::ok();
  }

  zx::result<> BytesReadyImpl(uint64_t bytes_written) {
    if (bytes_written > kRingBufferSize) {
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }
    if (!vmo_.is_valid()) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
    if (!expected_size_.has_value()) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
    uint64_t expected_size = *expected_size_;
    if (total_written_ + bytes_written > expected_size) {
      return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
    }
    uint64_t vmo_offset = total_written_ % kRingBufferSize;

    // |bytes_ready| may wrap around the end of ring buffer.
    uint64_t to_write = std::min(bytes_written, kRingBufferSize - vmo_offset);
    if (zx::result result = WriteBufferToBlob(vmo_offset, to_write); result.is_error()) {
      return result.take_error();
    }
    if (to_write < bytes_written) {
      // Write the wrapped bytes.
      if (zx::result result = WriteBufferToBlob(0, bytes_written - to_write); result.is_error()) {
        return result.take_error();
      }
    }
    if (total_written_ == expected_size) {
      // The blob should now be fully written and in the readable state.
      ZX_DEBUG_ASSERT(blob_->IsReadable());
    }
    return zx::ok();
  }

  fbl::RefPtr<Blob> blob_;
  std::optional<uint64_t> expected_size_;
  uint64_t total_written_ = 0;
  zx::vmo vmo_;
  fzl::VmoMapper vmo_mapper_;
  uint8_t* buffer_ = nullptr;
};

}  // namespace

BlobCreator::BlobCreator(Blobfs& blobfs)
    : fs::Service([this](fidl::ServerEnd<fuchsia_fxfs::BlobCreator> server_end) {
        bindings_.AddBinding(blobfs_.dispatcher(), std::move(server_end), this,
                             fidl::kIgnoreBindingClosure);
        return ZX_OK;
      }),
      blobfs_(blobfs) {}

void BlobCreator::Create(fuchsia_fxfs::wire::BlobCreatorCreateRequest* request,
                         CreateCompleter::Sync& completer) {
  Digest digest(request->hash.data_);
  auto result = CreateImpl(digest, request->allow_existing);
  if (result.is_error()) {
    if (result.status_value() == ZX_ERR_ALREADY_EXISTS) {
      completer.ReplyError(fuchsia_fxfs::wire::CreateBlobError::kAlreadyExists);
    } else {
      completer.ReplyError(fuchsia_fxfs::wire::CreateBlobError::kInternal);
    }
  } else {
    completer.ReplySuccess(std::move(result).value());
  }
}

zx::result<fidl::ClientEnd<fuchsia_fxfs::BlobWriter>> BlobCreator::CreateImpl(const Digest& digest,
                                                                              bool allow_existing) {
  std::optional<fbl::RefPtr<Blob>> to_overwrite;
  if (allow_existing) {
    // Check the cache if a previous version is here. If so, save a ref to it to replace it. To keep
    // the number of states low, only allow this if the existing version is readable, and there is
    // not another overwrite already in-flight. Also blocks if the blob is already marked deleted
    // and has not yet been purged. It would be confusing to allow overwrite to start after a purge
    // has been queued, but will later purge both versions.
    fbl::RefPtr<CacheNode> found;
    if (zx_status_t status = blobfs_.GetCache().Lookup(digest, &found); status == ZX_OK) {
      auto blob = fbl::RefPtr<Blob>::Downcast(std::move(found));
      if (!blob->IsReadable() || blob->SetOverwriting() != ZX_OK || blob->DeletionQueued()) {
        return zx::error(ZX_ERR_ALREADY_EXISTS);
      }
      to_overwrite = std::move(blob);
    }
  }

  fbl::RefPtr new_blob = fbl::AdoptRef(new Blob(blobfs_, digest, true));
  if (to_overwrite.has_value()) {
    // Don't put it in the cache if it is not the canonical version yet.
    new_blob->SetBlobToOverwrite(std::move(to_overwrite.value()));
  } else {
    if (zx_status_t status = blobfs_.GetCache().Add(new_blob); status != ZX_OK) {
      return zx::error(status);
    }
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_fxfs::BlobWriter>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  auto writer = std::make_unique<BlobWriter>(std::move(new_blob));
  writer_bindings_.AddBinding(blobfs_.dispatcher(), std::move(endpoints->server), writer.get(),
                              [writer = std::move(writer)](fidl::UnbindInfo info) {
                                // The lambda owns the writer. When the binding is closed, the
                                // writer will be destroyed.
                              });
  return zx::ok(std::move(endpoints->client));
}

}  // namespace blobfs
