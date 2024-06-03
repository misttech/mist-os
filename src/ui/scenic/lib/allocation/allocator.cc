// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/allocation/allocator.h"

#include <lib/async/cpp/wait.h>
#include <lib/async/default.h>
#include <lib/fit/function.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>

#include <memory>

#include "src/lib/fsl/handles/object_info.h"
#include "src/ui/scenic/lib/allocation/buffer_collection_importer.h"

using allocation::BufferCollectionUsage;
using fuchsia::ui::composition::RegisterBufferCollectionError;
using fuchsia::ui::composition::RegisterBufferCollectionUsages;

namespace allocation {

namespace {

RegisterBufferCollectionUsages UsageToUsages(
    fuchsia::ui::composition::RegisterBufferCollectionUsage usage) {
  switch (usage) {
    case fuchsia::ui::composition::RegisterBufferCollectionUsage::DEFAULT:
      return fuchsia::ui::composition::RegisterBufferCollectionUsages::DEFAULT;
    case fuchsia::ui::composition::RegisterBufferCollectionUsage::SCREENSHOT:
      return fuchsia::ui::composition::RegisterBufferCollectionUsages::SCREENSHOT;
  }
}

bool BufferCollectionTokenIsValid(
    fuchsia::sysmem2::AllocatorSyncPtr& sysmem_allocator,
    const fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken>& token) {
  fuchsia::sysmem2::AllocatorValidateBufferCollectionTokenRequest validate_request;
  validate_request.set_token_server_koid(fsl::GetRelatedKoid(token.channel().get()));
  fuchsia::sysmem2::Allocator_ValidateBufferCollectionToken_Result validate_result;
  const auto status = sysmem_allocator->ValidateBufferCollectionToken(std::move(validate_request),
                                                                      &validate_result);
  return status == ZX_OK && validate_result.is_response() &&
         validate_result.response().has_is_known() && validate_result.response().is_known();
}

// Creates a vector of |num_tokens| duplicates of BufferCollectionTokenSyncPtr.
// Returns an empty vector if creation failed.
std::vector<fuchsia::sysmem2::BufferCollectionTokenSyncPtr> CreateVectorOfTokens(
    fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> token, const size_t num_tokens) {
  FX_DCHECK(num_tokens > 0);
  std::vector<fuchsia::sysmem2::BufferCollectionTokenSyncPtr> tokens;
  tokens.emplace_back(token.BindSync());

  fuchsia::sysmem2::BufferCollectionTokenDuplicateSyncRequest dup_sync_request;
  dup_sync_request.set_rights_attenuation_masks(
      std::vector<zx_rights_t>(num_tokens - 1, ZX_RIGHT_SAME_RIGHTS));
  fuchsia::sysmem2::BufferCollectionToken_DuplicateSync_Result dup_sync_result;
  if (tokens.front()->DuplicateSync(std::move(dup_sync_request), &dup_sync_result) == ZX_OK &&
      dup_sync_result.is_response()) {
    // if is_response(), sysmem always fills out tokens vector (can be 0 length if we passed
    // 0-length rights_attenuation_masks above)
    FX_DCHECK(dup_sync_result.response().has_tokens());
    for (auto& token : *dup_sync_result.response().mutable_tokens()) {
      tokens.emplace_back(token.BindSync());
    }
  } else {
    tokens.clear();
  }

  return tokens;
}

struct ParsedArgs {
  zx_koid_t koid;
  RegisterBufferCollectionUsages buffer_collection_usages;
  fuchsia::ui::composition::BufferCollectionExportToken export_token;
  fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken> buffer_collection_token;
};

// Parses the FIDL struct, validating the arguments. Logs an error and returns std::nullopt on
// failure.
std::optional<ParsedArgs> ParseArgs(fuchsia::ui::composition::RegisterBufferCollectionArgs args) {
  // It's okay if there's no specified RegisterBufferCollectionUsage. In that case, assume it is
  // DEFAULT.
  if (!args.has_buffer_collection_token() || !args.has_export_token()) {
    FX_LOGS(ERROR) << "RegisterBufferCollection called with missing arguments";
    return std::nullopt;
  }

  if (!args.buffer_collection_token().is_valid()) {
    FX_LOGS(ERROR) << "RegisterBufferCollection called with invalid buffer collection token";
    return std::nullopt;
  }

  if (!args.export_token().value.is_valid()) {
    FX_LOGS(ERROR) << "RegisterBufferCollection called with invalid export token";
    return std::nullopt;
  }

  // Check if there is a valid peer.
  if (fsl::GetRelatedKoid(args.export_token().value.get()) == ZX_KOID_INVALID) {
    FX_LOGS(ERROR) << "RegisterBufferCollection called with no valid import tokens";
    return std::nullopt;
  }

  if (args.has_usages() && args.usages().has_unknown_bits()) {
    FX_LOGS(ERROR) << "Arguments contain unknown BufferCollectionUsage type";
    return std::nullopt;
  }

  // Grab object koid to be used as unique_id.
  const GlobalBufferCollectionId koid = fsl::GetKoid(args.export_token().value.get());
  FX_DCHECK(koid != ZX_KOID_INVALID);

  // If no usages are set we default to DEFAULT. Otherwise the newer "usages" value takes precedence
  // over the deprecated "usage" variant.
  RegisterBufferCollectionUsages buffer_collection_usages = RegisterBufferCollectionUsages::DEFAULT;
  if (args.has_usages()) {
    buffer_collection_usages = args.usages();
  } else if (args.has_usage()) {
    buffer_collection_usages = UsageToUsages(args.usage());
  }

  return ParsedArgs{
      .koid = koid,
      .buffer_collection_usages = buffer_collection_usages,
      .export_token = std::move(*args.mutable_export_token()),
      .buffer_collection_token = fidl::InterfaceHandle<fuchsia::sysmem2::BufferCollectionToken>(
          args.mutable_buffer_collection_token()->TakeChannel()),
  };
}

}  // namespace

Allocator::Allocator(sys::ComponentContext* app_context,
                     const std::vector<std::shared_ptr<BufferCollectionImporter>>&
                         default_buffer_collection_importers,
                     const std::vector<std::shared_ptr<BufferCollectionImporter>>&
                         screenshot_buffer_collection_importers,
                     fuchsia::sysmem2::AllocatorSyncPtr sysmem_allocator)
    : dispatcher_(async_get_default_dispatcher()),
      default_buffer_collection_importers_(default_buffer_collection_importers),
      screenshot_buffer_collection_importers_(screenshot_buffer_collection_importers),
      sysmem_allocator_(std::move(sysmem_allocator)),
      weak_factory_(this) {
  FX_DCHECK(app_context);
  app_context->outgoing()->AddPublicService(bindings_.GetHandler(this));
}

Allocator::~Allocator() {
  FX_DCHECK(dispatcher_ == async_get_default_dispatcher());

  // Allocator outlives |*_buffer_collection_importers_| instances, because we hold shared_ptrs. It
  // is safe to release all remaining buffer collections because there should be no more usage.
  while (!buffer_collections_.empty()) {
    ReleaseBufferCollection(buffer_collections_.begin()->first);
  }
}

void Allocator::RegisterBufferCollection(
    fuchsia::ui::composition::RegisterBufferCollectionArgs args,
    RegisterBufferCollectionCallback callback) {
  TRACE_DURATION("gfx", "allocation::Allocator::RegisterBufferCollection");
  FX_DCHECK(dispatcher_ == async_get_default_dispatcher());

  auto parsed_args = ParseArgs(std::move(args));
  if (!parsed_args) {
    callback(fpromise::error(RegisterBufferCollectionError::BAD_OPERATION));
    return;
  }

  auto& [koid, buffer_collection_usages, export_token, buffer_collection_token] =
      parsed_args.value();

  // Check if this export token has already been used.
  if (buffer_collections_.find(koid) != buffer_collections_.end()) {
    FX_LOGS(ERROR) << "RegisterBufferCollection called with pre-registered export token";
    callback(fpromise::error(RegisterBufferCollectionError::BAD_OPERATION));
    return;
  }

  if (!BufferCollectionTokenIsValid(sysmem_allocator_, buffer_collection_token)) {
    FX_LOGS(ERROR) << "RegisterBufferCollection called with a buffer collection token where "
                      "ValidateBufferCollectionToken() failed";
    callback(fpromise::error(RegisterBufferCollectionError::BAD_OPERATION));
    return;
  }

  const auto importers = GetImporters(buffer_collection_usages);
  // Create a token for each of the buffer collection importers.
  auto tokens = CreateVectorOfTokens(std::move(buffer_collection_token), importers.size());

  if (tokens.empty()) {
    FX_LOGS(ERROR) << "RegisterBufferCollection called with a buffer collection token where "
                      "Duplicate() failed";
    callback(fpromise::error(RegisterBufferCollectionError::BAD_OPERATION));
    return;
  }

  // Loop over each of the importers and provide each of them with a token from the vector we
  // created above.
  for (uint32_t i = 0; i < importers.size(); i++) {
    bool import_successful = false;
    {
      auto& [importer, usage] = importers.at(i);
      import_successful = importer.ImportBufferCollection(
          koid, sysmem_allocator_.get(), std::move(tokens[i]), usage, std::nullopt);
    }

    if (!import_successful) {
      // If any importers failed then clean up the ones that didn't before returning.
      for (uint32_t j = 0; j < i; j++) {
        auto& [importer, usage] = importers.at(j);
        importer.ReleaseBufferCollection(koid, usage);
      }
      FX_LOGS(ERROR) << "Failed to import the buffer collection to the BufferCollectionimporter.";
      callback(fpromise::error(RegisterBufferCollectionError::BAD_OPERATION));
      return;
    }
  }

  buffer_collections_[koid] = buffer_collection_usages;

  // Use a self-referencing async::WaitOnce to deregister buffer collections when all
  // BufferCollectionImportTokens are used, i.e. peers of eventpair are closed. Note that the
  // ownership of |export_token| is also passed, so that GetRelatedKoid() calls return valid koid.
  auto wait = std::make_shared<async::WaitOnce>(export_token.value.get(), ZX_EVENTPAIR_PEER_CLOSED);
  const zx_status_t status =
      wait->Begin(async_get_default_dispatcher(),
                  [keepalive_wait = wait, keepalive_export_token = std::move(export_token.value),
                   weak_ptr = weak_factory_.GetWeakPtr(),
                   koid = koid](async_dispatcher_t*, async::WaitOnce*, zx_status_t status,
                                const zx_packet_signal_t* /*signal*/) mutable {
                    FX_DCHECK(status == ZX_OK || status == ZX_ERR_CANCELED);
                    if (!weak_ptr)
                      return;
                    // Because Flatland::CreateImage() holds an import token, this
                    // is guaranteed to be called after all images are created, so
                    // it is safe to release buffer collection.
                    weak_ptr->ReleaseBufferCollection(koid);
                  });
  FX_DCHECK(status == ZX_OK);

  callback(fpromise::ok());
}

std::vector<std::pair<BufferCollectionImporter&, BufferCollectionUsage>> Allocator::GetImporters(
    const RegisterBufferCollectionUsages usages) const {
  std::vector<std::pair<BufferCollectionImporter&, BufferCollectionUsage>> importers;
  if (usages & RegisterBufferCollectionUsages::DEFAULT) {
    for (const auto& importer : default_buffer_collection_importers_) {
      importers.emplace_back(*importer, BufferCollectionUsage::kClientImage);
    }
  }
  if (usages & RegisterBufferCollectionUsages::SCREENSHOT) {
    for (const auto& importer : screenshot_buffer_collection_importers_) {
      importers.emplace_back(*importer, BufferCollectionUsage::kRenderTarget);
    }
  }

  return importers;
}

void Allocator::ReleaseBufferCollection(GlobalBufferCollectionId collection_id) {
  TRACE_DURATION("gfx", "allocation::Allocator::ReleaseBufferCollection");
  FX_DCHECK(dispatcher_ == async_get_default_dispatcher());

  const auto usages = buffer_collections_.at(collection_id);
  buffer_collections_.erase(collection_id);

  for (auto& [importer, usage] : GetImporters(usages)) {
    importer.ReleaseBufferCollection(collection_id, usage);
  }
}

}  // namespace allocation
