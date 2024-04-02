// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "allocator.h"

#include <lib/ddk/trace/event.h>
#include <lib/fidl/internal.h>
#include <lib/zx/channel.h>
#include <lib/zx/event.h>
#include <zircon/fidl.h>

#include "logical_buffer_collection.h"

namespace sysmem_driver {

using Error = fuchsia_sysmem2::Error;

Allocator::Allocator(Device* parent_device)
    : LoggingMixin("allocator"), parent_device_(parent_device) {
  // nothing else to do here
}

Allocator::~Allocator() { LogInfo(FROM_HERE, "~Allocator"); }

// static
void Allocator::CreateChannelOwnedV1(zx::channel request, Device* device) {
  auto allocator = std::unique_ptr<Allocator>(new Allocator(device));
  auto v1_server = std::make_unique<V1>(std::move(allocator));
  // Ignore the result - allocator will be destroyed and the channel will be closed on error.
  fidl::BindServer(device->dispatcher(),
                   fidl::ServerEnd<fuchsia_sysmem::Allocator>(std::move(request)),
                   std::move(v1_server));
}

Allocator& Allocator::CreateChannelOwnedV2(zx::channel request, Device* device) {
  auto allocator = std::unique_ptr<Allocator>(new Allocator(device));
  auto allocator_ptr = allocator.get();
  auto v2_server = std::make_unique<V2>(std::move(allocator));
  // Ignore the result - allocator will be destroyed and the channel will be closed on error.
  fidl::BindServer(device->dispatcher(),
                   fidl::ServerEnd<fuchsia_sysmem2::Allocator>(std::move(request)),
                   std::move(v2_server));
  return *allocator_ptr;
}

template <typename Completer, typename Protocol>
fit::result<std::monostate, fidl::Endpoints<Protocol>> Allocator::CommonAllocateNonSharedCollection(
    Completer& completer) {
  // The AllocateCollection() message skips past the token stage because the
  // client is also the only participant (probably a temp/test client).  Real
  // clients are encouraged to use AllocateSharedCollection() instead, so that
  // the client can share the LogicalBufferCollection with other participants.
  //
  // Because this is a degenerate way to use sysmem, we implement this method
  // in terms of the non-degenerate way.
  //
  // This code is essentially the same as what a client would do if a client
  // wanted to skip the BufferCollectionToken stage without using
  // AllocateCollection().  Essentially, this code is here just so clients
  // that don't need to share their collection don't have to write this code,
  // and can share this code instead.

  // Create a local token.
  zx::result endpoints = fidl::CreateEndpoints<Protocol>();
  if (endpoints.is_error()) {
    LogError(FROM_HERE,
             "Allocator::AllocateCollection() zx::channel::create() failed "
             "- status: %d",
             endpoints.error_value());
    // ~buffer_collection_request
    //
    // Returning an error here causes the sysmem connection to drop also,
    // which seems like a good idea (more likely to recover overall) given
    // the nature of the error.
    completer.Close(ZX_ERR_INTERNAL);
    return fit::error(std::monostate{});
  }

  return fit::success(std::move(endpoints.value()));
}

void Allocator::V1::AllocateNonSharedCollection(
    AllocateNonSharedCollectionRequest& request,
    AllocateNonSharedCollectionCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Allocator::AllocateNonSharedCollection");

  fit::result endpoints = allocator_->CommonAllocateNonSharedCollection<
      decltype(completer), fuchsia_sysmem::BufferCollectionToken>(completer);
  if (!endpoints.is_ok()) {
    return;
  }
  auto& [token_client, token_server] = endpoints.value();

  // The server end of the local token goes to Create(), and the client end
  // goes to BindSharedCollection().  The BindSharedCollection() will figure
  // out which token we're talking about based on the koid(s), as usual.
  LogicalBufferCollection::CreateV1(
      std::move(token_server), allocator_->parent_device_,
      allocator_->client_debug_info_.has_value() ? &*allocator_->client_debug_info_ : nullptr);
  LogicalBufferCollection::BindSharedCollection(
      allocator_->parent_device_, token_client.TakeChannel(),
      std::move(request.collection_request()),
      allocator_->client_debug_info_.has_value() ? &*allocator_->client_debug_info_ : nullptr);

  // Now the client can SetConstraints() on the BufferCollection, etc.  The
  // client didn't have to hassle with the BufferCollectionToken, which is the
  // sole upside of the client using this message over
  // AllocateSharedCollection().
}

void Allocator::V2::AllocateNonSharedCollection(
    AllocateNonSharedCollectionRequest& request,
    AllocateNonSharedCollectionCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Allocator::AllocateNonSharedCollection");

  if (!request.collection_request().has_value()) {
    allocator_->LogError(FROM_HERE, "AllocateNonSharedCollection requires collection_request set");
    completer.Close(ZX_ERR_INTERNAL);
    return;
  }

  fit::result endpoints = allocator_->CommonAllocateNonSharedCollection<
      decltype(completer), fuchsia_sysmem2::BufferCollectionToken>(completer);
  if (!endpoints.is_ok()) {
    return;
  }
  auto& [token_client, token_server] = endpoints.value();

  // The server end of the local token goes to Create(), and the client end
  // goes to BindSharedCollection().  The BindSharedCollection() will figure
  // out which token we're talking about based on the koid(s), as usual.
  LogicalBufferCollection::CreateV2(
      std::move(token_server), allocator_->parent_device_,
      allocator_->client_debug_info_.has_value() ? &*allocator_->client_debug_info_ : nullptr);
  LogicalBufferCollection::BindSharedCollection(
      allocator_->parent_device_, token_client.TakeChannel(),
      std::move(request.collection_request().value()),
      allocator_->client_debug_info_.has_value() ? &*allocator_->client_debug_info_ : nullptr);

  // Now the client can SetConstraints() on the BufferCollection, etc.  The
  // client didn't have to hassle with the BufferCollectionToken, which is the
  // sole upside of the client using this message over
  // AllocateSharedCollection().
}

void Allocator::V1::AllocateSharedCollection(AllocateSharedCollectionRequest& request,
                                             AllocateSharedCollectionCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Allocator::AllocateSharedCollection");

  // The LogicalBufferCollection is self-owned / owned by all the channels it
  // serves.
  //
  // There's no channel served directly by the LogicalBufferCollection.
  // Instead LogicalBufferCollection owns all the FidlServer instances that
  // each own a channel.
  //
  // Initially there's only a channel to the first BufferCollectionToken.  We
  // go ahead and allocate the LogicalBufferCollection here since the
  // LogicalBufferCollection associates all the BufferCollectionToken and
  // BufferCollection bindings to the same LogicalBufferCollection.
  LogicalBufferCollection::CreateV1(
      std::move(request.token_request()), allocator_->parent_device_,
      allocator_->client_debug_info_.has_value() ? &*allocator_->client_debug_info_ : nullptr);
}

void Allocator::V2::AllocateSharedCollection(AllocateSharedCollectionRequest& request,
                                             AllocateSharedCollectionCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Allocator::AllocateSharedCollection");

  if (!request.token_request().has_value()) {
    allocator_->LogError(FROM_HERE, "AllocateSharedCollection requires token_request set");
    completer.Close(ZX_ERR_INTERNAL);
    return;
  }

  // The LogicalBufferCollection is self-owned / owned by all the channels it
  // serves.
  //
  // There's no channel served directly by the LogicalBufferCollection.
  // Instead LogicalBufferCollection owns all the FidlServer instances that
  // each own a channel.
  //
  // Initially there's only a channel to the first BufferCollectionToken.  We
  // go ahead and allocate the LogicalBufferCollection here since the
  // LogicalBufferCollection associates all the BufferCollectionToken and
  // BufferCollection bindings to the same LogicalBufferCollection.
  LogicalBufferCollection::CreateV2(
      std::move(request.token_request().value()), allocator_->parent_device_,
      allocator_->client_debug_info_.has_value() ? &*allocator_->client_debug_info_ : nullptr);
}

void Allocator::V1::BindSharedCollection(BindSharedCollectionRequest& request,
                                         BindSharedCollectionCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Allocator::BindSharedCollection");

  // The BindSharedCollection() message is about a supposed-to-be-pre-existing
  // logical BufferCollection, but the only association we have to that
  // BufferCollection is the client end of a BufferCollectionToken channel
  // being handed in via token_param.  To find any associated BufferCollection
  // we have to look it up by koid.  The koid table is held by
  // LogicalBufferCollection, so delegate over to LogicalBufferCollection for
  // this request.
  LogicalBufferCollection::BindSharedCollection(
      allocator_->parent_device_, request.token().TakeChannel(),
      std::move(request.buffer_collection_request()),
      allocator_->client_debug_info_.has_value() ? &*allocator_->client_debug_info_ : nullptr);
}

void Allocator::V2::BindSharedCollection(BindSharedCollectionRequest& request,
                                         BindSharedCollectionCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Allocator::BindSharedCollection");

  if (!request.token().has_value()) {
    allocator_->LogError(FROM_HERE, "BindSharedCollection requires token set");
    completer.Close(ZX_ERR_INTERNAL);
    return;
  }

  if (!request.buffer_collection_request().has_value()) {
    allocator_->LogError(FROM_HERE, "BindSharedCollection requires buffer_collection_request set");
    completer.Close(ZX_ERR_INTERNAL);
    return;
  }

  // The BindSharedCollection() message is about a supposed-to-be-pre-existing
  // logical BufferCollection, but the only association we have to that
  // BufferCollection is the client end of a BufferCollectionToken channel
  // being handed in via token_param.  To find any associated BufferCollection
  // we have to look it up by koid.  The koid table is held by
  // LogicalBufferCollection, so delegate over to LogicalBufferCollection for
  // this request.
  LogicalBufferCollection::BindSharedCollection(
      allocator_->parent_device_, request.token()->TakeChannel(),
      std::move(request.buffer_collection_request().value()),
      allocator_->client_debug_info_.has_value() ? &*allocator_->client_debug_info_ : nullptr);
}

void Allocator::V1::ValidateBufferCollectionToken(
    ValidateBufferCollectionTokenRequest& request,
    ValidateBufferCollectionTokenCompleter::Sync& completer) {
  zx_status_t status = LogicalBufferCollection::ValidateBufferCollectionToken(
      allocator_->parent_device_, request.token_server_koid());
  ZX_DEBUG_ASSERT(status == ZX_OK || status == ZX_ERR_NOT_FOUND);
  completer.Reply(status == ZX_OK);
}

void Allocator::V2::ValidateBufferCollectionToken(
    ValidateBufferCollectionTokenRequest& request,
    ValidateBufferCollectionTokenCompleter::Sync& completer) {
  if (!request.token_server_koid().has_value()) {
    allocator_->LogError(FROM_HERE, "ValidateBufferCollectionToken requires token_server_koid set");
    completer.Close(ZX_ERR_INTERNAL);
    return;
  }
  zx_status_t status = LogicalBufferCollection::ValidateBufferCollectionToken(
      allocator_->parent_device_, request.token_server_koid().value());
  ZX_DEBUG_ASSERT(status == ZX_OK || status == ZX_ERR_NOT_FOUND);
  fuchsia_sysmem2::AllocatorValidateBufferCollectionTokenResponse response;
  response.is_known().emplace(status == ZX_OK);
  completer.Reply(std::move(response));
}

void Allocator::V1::SetDebugClientInfo(SetDebugClientInfoRequest& request,
                                       SetDebugClientInfoCompleter::Sync& completer) {
  allocator_->client_debug_info_.emplace();
  allocator_->client_debug_info_->name = std::string(request.name().begin(), request.name().end());
  allocator_->client_debug_info_->id = request.id();
}

void Allocator::V2::SetDebugClientInfo(SetDebugClientInfoRequest& request,
                                       SetDebugClientInfoCompleter::Sync& completer) {
  if (!request.name().has_value()) {
    allocator_->LogError(FROM_HERE, "SetDebugClientInfo requires name set");
    completer.Close(ZX_ERR_INTERNAL);
    return;
  }
  uint64_t id = 0;
  if (request.id().has_value()) {
    id = *request.id();
  }
  allocator_->client_debug_info_.emplace();
  allocator_->client_debug_info_->name =
      std::string(request.name()->begin(), request.name()->end());
  allocator_->client_debug_info_->id = id;
}

void Allocator::V1::ConnectToSysmem2Allocator(ConnectToSysmem2AllocatorRequest& request,
                                              ConnectToSysmem2AllocatorCompleter::Sync& completer) {
  auto v2_allocator = Allocator::CreateChannelOwnedV2(request.allocator_request().TakeChannel(),
                                                      allocator_->parent_device_);
  if (allocator_->client_debug_info_.has_value()) {
    // intentional clone / copy
    v2_allocator.client_debug_info_ = *allocator_->client_debug_info_;
  }
}

void Allocator::V2::GetVmoInfo(GetVmoInfoRequest& request, GetVmoInfoCompleter::Sync& completer) {
  if (!request.vmo().has_value()) {
    allocator_->LogError(FROM_HERE, "GetVmoInfo requires vmo handle (!has_value)");
    completer.Reply(fit::error(Error::kProtocolDeviation));
    return;
  }
  if (!request.vmo()->is_valid()) {
    allocator_->LogError(FROM_HERE, "GetVmoInfo requires vmo handle (!is_valid)");
    completer.Reply(fit::error(Error::kProtocolDeviation));
    return;
  }
  auto& vmo = *request.vmo();
  zx_info_handle_basic_t basic_info{};
  zx_status_t status =
      vmo.get_info(ZX_INFO_HANDLE_BASIC, &basic_info, sizeof(basic_info), nullptr, nullptr);
  if (status != ZX_OK) {
    allocator_->LogError(FROM_HERE, "GetVmoInfo couldn't vmo.get_info to get koid");

    Error translated_status;
    if (status == ZX_ERR_ACCESS_DENIED) {
      translated_status = Error::kHandleAccessDenied;
    } else {
      translated_status = Error::kUnspecified;
    }

    completer.Reply(fit::error(translated_status));
    return;
  }
  // Possibly redundant with FIDL generated code.
  if (basic_info.type != ZX_OBJ_TYPE_VMO) {
    allocator_->LogError(FROM_HERE, "GetVmoInfo requires VMO handle");
    completer.Reply(fit::error(Error::kProtocolDeviation));
    return;
  }
  zx_koid_t vmo_koid = basic_info.koid;
  auto logical_buffer_result = allocator_->parent_device_->FindLogicalBufferByVmoKoid(vmo_koid);
  if (!logical_buffer_result.logical_buffer) {
    // We don't log anything in this path because a client may just be checking if a VMO is a
    // sysmem VMO, which could make a LogInfo() here noisy.
    completer.Reply(fit::error(Error::kNotFound));
    return;
  }
  auto& logical_buffer = *logical_buffer_result.logical_buffer;
  fuchsia_sysmem2::AllocatorGetVmoInfoResponse response;
  response.buffer_collection_id() =
      logical_buffer.logical_buffer_collection().buffer_collection_id();
  response.buffer_index() = logical_buffer.buffer_index();
  if (logical_buffer_result.is_koid_of_weak_vmo) {
    auto dup_result = logical_buffer.logical_buffer_collection().DupCloseWeakAsapClientEnd(
        logical_buffer.buffer_index());
    if (dup_result.is_error()) {
      completer.Reply(fit::error{Error::kUnspecified});
      return;
    }
    response.close_weak_asap() = std::move(dup_result.value());
  }
  completer.Reply(fit::ok(std::move(response)));
}

void Allocator::V2::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_sysmem2::Allocator> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  allocator_->LogError(FROM_HERE, "token group unknown method - ordinal: %" PRIx64,
                       metadata.method_ordinal);
  completer.Close(ZX_ERR_INTERNAL);
}

}  // namespace sysmem_driver
