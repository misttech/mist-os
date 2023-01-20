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

void Allocator::CreateChannelOwnedV2(zx::channel request, Device* device) {
  auto allocator = std::unique_ptr<Allocator>(new Allocator(device));
  auto v2_server = std::make_unique<V2>(std::move(allocator));
  // Ignore the result - allocator will be destroyed and the channel will be closed on error.
  fidl::BindServer(device->dispatcher(),
                   fidl::ServerEnd<fuchsia_sysmem2::Allocator>(std::move(request)),
                   std::move(v2_server));
}

template <typename Completer>
fit::result<std::monostate, std::pair<zx::channel, zx::channel>>
Allocator::CommonAllocateNonSharedCollection(Completer& completer) {
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
  zx::channel token_client;
  zx::channel token_server;
  zx_status_t status = zx::channel::create(0, &token_client, &token_server);
  if (status != ZX_OK) {
    LogError(FROM_HERE,
             "Allocator::AllocateCollection() zx::channel::create() failed "
             "- status: %d",
             status);
    // ~buffer_collection_request
    //
    // Returning an error here causes the sysmem connection to drop also,
    // which seems like a good idea (more likely to recover overall) given
    // the nature of the error.
    completer.Close(status);
    return fit::error(std::monostate{});
  }

  return fit::success(std::make_pair(std::move(token_client), std::move(token_server)));
}

void Allocator::V1::AllocateNonSharedCollection(
    AllocateNonSharedCollectionRequest& request,
    AllocateNonSharedCollectionCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Allocator::AllocateNonSharedCollection");

  auto result = allocator_->CommonAllocateNonSharedCollection(completer);
  if (!result.is_ok()) {
    return;
  }
  auto endpoints = std::move(result.value());
  auto& [token_client, token_server] = endpoints;

  // The server end of the local token goes to Create(), and the client end
  // goes to BindSharedCollection().  The BindSharedCollection() will figure
  // out which token we're talking about based on the koid(s), as usual.
  LogicalBufferCollection::CreateV1(std::move(token_server), allocator_->parent_device_);
  LogicalBufferCollection::BindSharedCollection(
      allocator_->parent_device_, std::move(token_client), std::move(request.collection_request()),
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
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }

  auto result = allocator_->CommonAllocateNonSharedCollection(completer);
  if (!result.is_ok()) {
    return;
  }
  auto endpoints = std::move(result.value());
  auto [token_client, token_server] = std::move(endpoints);

  // The server end of the local token goes to Create(), and the client end
  // goes to BindSharedCollection().  The BindSharedCollection() will figure
  // out which token we're talking about based on the koid(s), as usual.
  LogicalBufferCollection::CreateV2(std::move(token_server), allocator_->parent_device_);
  LogicalBufferCollection::BindSharedCollection(
      allocator_->parent_device_, std::move(token_client),
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
  LogicalBufferCollection::CreateV1(request.token_request().TakeChannel(),
                                    allocator_->parent_device_);
}

void Allocator::V2::AllocateSharedCollection(AllocateSharedCollectionRequest& request,
                                             AllocateSharedCollectionCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "Allocator::AllocateSharedCollection");

  if (!request.token_request().has_value()) {
    allocator_->LogError(FROM_HERE, "AllocateSharedCollection requires token_request set");
    completer.Close(ZX_ERR_INVALID_ARGS);
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
  LogicalBufferCollection::CreateV2(request.token_request()->TakeChannel(),
                                    allocator_->parent_device_);
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
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (!request.buffer_collection_request().has_value()) {
    allocator_->LogError(FROM_HERE, "BindSharedCollection requires buffer_collection_request set");
    completer.Close(ZX_ERR_INVALID_ARGS);
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
    completer.Close(ZX_ERR_INVALID_ARGS);
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
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  if (!request.id().has_value()) {
    allocator_->LogError(FROM_HERE, "SetDebugClientInfo requires id set");
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  allocator_->client_debug_info_.emplace();
  allocator_->client_debug_info_->name =
      std::string(request.name()->begin(), request.name()->end());
  allocator_->client_debug_info_->id = request.id().value();
}

}  // namespace sysmem_driver
