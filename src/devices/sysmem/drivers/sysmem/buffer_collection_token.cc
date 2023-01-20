// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "buffer_collection_token.h"

#include <lib/ddk/trace/event.h>
#include <lib/fidl-utils/bind.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/zx/channel.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include "device.h"
#include "node.h"
#include "node_properties.h"

namespace sysmem_driver {

BufferCollectionToken::~BufferCollectionToken() {
  TRACE_DURATION("gfx", "BufferCollectionToken::~BufferCollectionToken", "this", this,
                 "logical_buffer_collection", &logical_buffer_collection());

  // zx_koid_t values are never re-used during lifetime of running system, so
  // it's fine that the channel is already closed (no possibility of re-use
  // of value in the tracked set of values).

  // It's fine if server_koid() is ZX_KOID_INVALID - no effect in that case.
  parent_device()->UntrackToken(this);
}

void BufferCollectionToken::CloseServerBinding(zx_status_t epitaph) {
  if (server_binding_v1_.has_value()) {
    server_binding_v1_->Close(epitaph);
  }
  if (server_binding_v2_.has_value()) {
    server_binding_v2_->Close(epitaph);
  }
  server_binding_v1_ = {};
  server_binding_v2_ = {};
  parent_device()->UntrackToken(this);
}

// static
BufferCollectionToken& BufferCollectionToken::EmplaceInTree(
    fbl::RefPtr<LogicalBufferCollection> logical_buffer_collection,
    NodeProperties* new_node_properties, const TokenServerEnd& server_end) {
  auto token = fbl::AdoptRef(new BufferCollectionToken(std::move(logical_buffer_collection),
                                                       new_node_properties, server_end));
  auto token_ptr = token.get();
  new_node_properties->SetNode(token);
  return *token_ptr;
}

void BufferCollectionToken::Bind(TokenServerEnd token_server_end) {
  Node::Bind(TakeNodeServerEnd(std::move(token_server_end)));
}

void BufferCollectionToken::BindInternalV1(zx::channel token_request,
                                           ErrorHandlerWrapper error_handler_wrapper) {
  v1_server_.emplace(*this);
  server_binding_v1_ = fidl::BindServer(
      parent_device()->dispatcher(),
      fidl::ServerEnd<fuchsia_sysmem::BufferCollectionToken>(std::move(token_request)),
      &v1_server_.value(),
      [error_handler_wrapper = std::move(error_handler_wrapper)](
          BufferCollectionToken::V1* token, fidl::UnbindInfo info, TokenServerEndV1 channel) {
        error_handler_wrapper(info);
      });
}

void BufferCollectionToken::BindInternalV2(zx::channel token_request,
                                           ErrorHandlerWrapper error_handler_wrapper) {
  v2_server_.emplace(*this);
  server_binding_v2_ = fidl::BindServer(
      parent_device()->dispatcher(),
      fidl::ServerEnd<fuchsia_sysmem2::BufferCollectionToken>(std::move(token_request)),
      &v2_server_.value(),
      [error_handler_wrapper = std::move(error_handler_wrapper)](
          BufferCollectionToken::V2* token, fidl::UnbindInfo info, TokenServerEndV2 channel) {
        error_handler_wrapper(info);
      });
}

void BufferCollectionToken::V1::DuplicateSync(DuplicateSyncRequest& request,
                                              DuplicateSyncCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollectionToken::V1::DuplicateSync", "this", this,
                 "logical_buffer_collection", &parent_.logical_buffer_collection());
  if (parent_.is_done_) {
    // Probably a Close() followed by DuplicateSync(), which is illegal and
    // causes the whole LogicalBufferCollection to fail.
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
                     "BufferCollectionToken::DuplicateSync() attempted when is_done_");
    return;
  }
  std::vector<fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>> new_tokens;

  for (auto& rights_attenuation_mask : request.rights_attenuation_masks()) {
    auto token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
    if (!token_endpoints.is_ok()) {
      parent_.FailSync(FROM_HERE, completer, token_endpoints.status_value(),
                       "BufferCollectionToken::DuplicateSync() failed to create token channel.");
      return;
    }

    NodeProperties* new_node_properties =
        parent_.node_properties().NewChild(&parent_.logical_buffer_collection());
    if (rights_attenuation_mask != ZX_RIGHT_SAME_RIGHTS) {
      new_node_properties->rights_attenuation_mask() &=
          safe_cast<uint32_t>(rights_attenuation_mask);
    }
    parent_.logical_buffer_collection().CreateBufferCollectionTokenV1(
        parent_.shared_logical_buffer_collection(), new_node_properties,
        std::move(token_endpoints->server));
    new_tokens.push_back(std::move(token_endpoints->client));
  }

  fuchsia_sysmem::BufferCollectionTokenDuplicateSyncResponse response;
  response.tokens() = std::move(new_tokens);
  completer.Reply(std::move(response));
}

void BufferCollectionToken::V2::DuplicateSync(DuplicateSyncRequest& request,
                                              DuplicateSyncCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollectionToken::V2::DuplicateSync", "this", this,
                 "logical_buffer_collection", &parent_.logical_buffer_collection());
  if (parent_.is_done_) {
    // Probably a Close() followed by DuplicateSync(), which is illegal and
    // causes the whole LogicalBufferCollection to fail.
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
                     "DuplicateSync() attempted when is_done_");
    return;
  }
  if (!request.rights_attenuation_masks().has_value()) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
                     "DuplicateSync() requires rights_attenuation_masks set");
    return;
  }

  std::vector<fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken>> new_tokens;
  for (auto& rights_attenuation_mask : request.rights_attenuation_masks().value()) {
    auto token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
    if (!token_endpoints.is_ok()) {
      parent_.FailSync(FROM_HERE, completer, token_endpoints.status_value(),
                       "BufferCollectionToken::DuplicateSync() failed to create token channel.");
      return;
    }

    NodeProperties* new_node_properties =
        parent_.node_properties().NewChild(&parent_.logical_buffer_collection());
    if (rights_attenuation_mask != ZX_RIGHT_SAME_RIGHTS) {
      new_node_properties->rights_attenuation_mask() &=
          safe_cast<uint32_t>(rights_attenuation_mask);
    }
    parent_.logical_buffer_collection().CreateBufferCollectionTokenV2(
        parent_.shared_logical_buffer_collection(), new_node_properties,
        std::move(token_endpoints->server));
    new_tokens.push_back(std::move(token_endpoints->client));
  }

  fuchsia_sysmem2::BufferCollectionTokenDuplicateSyncResponse response;
  response.tokens() = std::move(new_tokens);
  completer.Reply(std::move(response));
}

template <typename Completer>
bool BufferCollectionToken::CommonDuplicateStage1(uint32_t rights_attenuation_mask,
                                                  Completer& completer,
                                                  NodeProperties** out_node_properties) {
  if (is_done_) {
    // Probably a Close() followed by Duplicate(), which is illegal and
    // causes the whole LogicalBufferCollection to fail.
    FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
             "BufferCollectionToken::Duplicate() attempted when is_done_");
    return false;
  }
  NodeProperties* new_node_properties = node_properties().NewChild(&logical_buffer_collection());
  if (rights_attenuation_mask == 0) {
    logical_buffer_collection().LogClientError(
        FROM_HERE, &node_properties(),
        "rights_attenuation_mask of 0 is DEPRECATED - use ZX_RIGHT_SAME_RIGHTS instead.");
    rights_attenuation_mask = ZX_RIGHT_SAME_RIGHTS;
  }
  if (rights_attenuation_mask != ZX_RIGHT_SAME_RIGHTS) {
    new_node_properties->rights_attenuation_mask() &= rights_attenuation_mask;
  }
  *out_node_properties = new_node_properties;
  return true;
}

void BufferCollectionToken::V1::Duplicate(DuplicateRequest& request,
                                          DuplicateCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollectionToken::Duplicate", "this", this,
                 "logical_buffer_collection", &parent_.logical_buffer_collection());
  NodeProperties* new_node_properties;
  if (!parent_.CommonDuplicateStage1(request.rights_attenuation_mask(), completer,
                                     &new_node_properties)) {
    return;
  }
  parent_.logical_buffer_collection().CreateBufferCollectionTokenV1(
      parent_.shared_logical_buffer_collection(), new_node_properties,
      std::move(request.token_request()));
}

void BufferCollectionToken::V2::Duplicate(DuplicateRequest& request,
                                          DuplicateCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollectionToken::Duplicate", "this", this,
                 "logical_buffer_collection", &parent_.logical_buffer_collection());
  if (!request.rights_attenuation_mask().has_value()) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
                     "Duplicate() requires rights_attenuation_mask set");
    return;
  }
  if (!request.token_request().has_value()) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
                     "Duplicate() requires token_request set");
    return;
  }
  NodeProperties* new_node_properties;
  if (!parent_.CommonDuplicateStage1(request.rights_attenuation_mask().value(), completer,
                                     &new_node_properties)) {
    return;
  }
  parent_.logical_buffer_collection().CreateBufferCollectionTokenV2(
      parent_.shared_logical_buffer_collection(), new_node_properties,
      std::move(request.token_request().value()));
}

void BufferCollectionToken::V1::Sync(SyncCompleter::Sync& completer) {
  parent_.SyncImpl(completer);
}

void BufferCollectionToken::V2::Sync(SyncCompleter::Sync& completer) {
  parent_.SyncImpl(completer);
}

void BufferCollectionToken::V1::DeprecatedSync(DeprecatedSyncCompleter::Sync& completer) {
  parent_.SyncImpl(completer);
}

// Clean token close without causing LogicalBufferCollection failure.
void BufferCollectionToken::V1::Close(CloseCompleter::Sync& completer) {
  parent_.TokenCloseImpl(completer);
}

void BufferCollectionToken::V2::Close(CloseCompleter::Sync& completer) {
  parent_.TokenCloseImpl(completer);
}

void BufferCollectionToken::V1::DeprecatedClose(DeprecatedCloseCompleter::Sync& completer) {
  parent_.TokenCloseImpl(completer);
}

void BufferCollectionToken::OnServerKoid() {
  ZX_DEBUG_ASSERT(has_server_koid());
  parent_device()->TrackToken(this);
  if (parent_device()->TryRemoveKoidFromUnfoundTokenList(server_koid())) {
    set_unfound_node();
    // LogicalBufferCollection will print an error, since it might have useful client information.
  }
}

bool BufferCollectionToken::is_done() { return is_done_; }

void BufferCollectionToken::SetBufferCollectionRequest(
    CollectionServerEnd buffer_collection_request) {
  if (is_done_ || buffer_collection_request_.has_value()) {
    FailAsync(FROM_HERE, ZX_ERR_BAD_STATE,
              "BufferCollectionToken::SetBufferCollectionRequest() attempted "
              "when already is_done_ || buffer_collection_request_");
    return;
  }
  ZX_DEBUG_ASSERT(!buffer_collection_request_);
  buffer_collection_request_ = std::move(buffer_collection_request);
}

std::optional<CollectionServerEnd> BufferCollectionToken::TakeBufferCollectionRequest() {
  return std::exchange(buffer_collection_request_, std::nullopt);
}

void BufferCollectionToken::V1::SetName(SetNameRequest& request,
                                        SetNameCompleter::Sync& completer) {
  parent_.SetNameImplV1(request, completer);
}

void BufferCollectionToken::V2::SetName(SetNameRequest& request,
                                        SetNameCompleter::Sync& completer) {
  parent_.SetNameImplV2(request, completer);
}

void BufferCollectionToken::V1::DeprecatedSetName(DeprecatedSetNameRequest& request,
                                                  DeprecatedSetNameCompleter::Sync& completer) {
  parent_.SetNameImplV1(request, completer);
}

void BufferCollectionToken::V1::SetDebugClientInfo(SetDebugClientInfoRequest& request,
                                                   SetDebugClientInfoCompleter::Sync& completer) {
  parent_.SetDebugClientInfoImplV1(request, completer);
}

void BufferCollectionToken::V2::SetDebugClientInfo(SetDebugClientInfoRequest& request,
                                                   SetDebugClientInfoCompleter::Sync& completer) {
  parent_.SetDebugClientInfoImplV2(request, completer);
}

void BufferCollectionToken::V1::DeprecatedSetDebugClientInfo(
    DeprecatedSetDebugClientInfoRequest& request,
    DeprecatedSetDebugClientInfoCompleter::Sync& completer) {
  parent_.SetDebugClientInfoImplV1(request, completer);
}

void BufferCollectionToken::V1::SetDebugTimeoutLogDeadline(
    SetDebugTimeoutLogDeadlineRequest& request,
    SetDebugTimeoutLogDeadlineCompleter::Sync& completer) {
  parent_.SetDebugTimeoutLogDeadlineImplV1(request, completer);
}

void BufferCollectionToken::V2::SetDebugTimeoutLogDeadline(
    SetDebugTimeoutLogDeadlineRequest& request,
    SetDebugTimeoutLogDeadlineCompleter::Sync& completer) {
  parent_.SetDebugTimeoutLogDeadlineImplV2(request, completer);
}

void BufferCollectionToken::V1::DeprecatedSetDebugTimeoutLogDeadline(
    DeprecatedSetDebugTimeoutLogDeadlineRequest& request,
    DeprecatedSetDebugTimeoutLogDeadlineCompleter::Sync& completer) {
  parent_.SetDebugTimeoutLogDeadlineImplV1(request, completer);
}

void BufferCollectionToken::V1::SetDispensable(SetDispensableCompleter::Sync& completer) {
  parent_.SetDispensableInternal();
}

void BufferCollectionToken::V2::SetDispensable(SetDispensableCompleter::Sync& completer) {
  parent_.SetDispensableInternal();
}

void BufferCollectionToken::SetDispensableInternal() {
  if (node_properties().error_propagation_mode() <
      ErrorPropagationMode::kPropagateBeforeAllocation) {
    node_properties().error_propagation_mode() = ErrorPropagationMode::kPropagateBeforeAllocation;
  }
}

template <typename Completer>
bool BufferCollectionToken::CommonCreateBufferCollectionTokenGroupStage1(
    Completer& completer, NodeProperties** out_node_properties) {
  if (is_done_) {
    // Probably a Close() followed by Duplicate(), which is illegal and
    // causes the whole LogicalBufferCollection to fail.
    FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
             "BufferCollectionToken::CreateBufferCollectionTokenGroup() attempted when is_done_");
    return false;
  }
  NodeProperties* new_node_properties = node_properties().NewChild(&logical_buffer_collection());
  *out_node_properties = new_node_properties;
  return true;
}

void BufferCollectionToken::V1::CreateBufferCollectionTokenGroup(
    CreateBufferCollectionTokenGroupRequest& request,
    CreateBufferCollectionTokenGroupCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollectionTokenGroup::CreateBufferCollectionTokenGroup", "this",
                 this, "logical_buffer_collection", &parent_.logical_buffer_collection());
  NodeProperties* new_node_properties;
  if (!parent_.CommonCreateBufferCollectionTokenGroupStage1(completer, &new_node_properties)) {
    return;
  }
  parent_.logical_buffer_collection().CreateBufferCollectionTokenGroupV1(
      parent_.shared_logical_buffer_collection(), new_node_properties,
      std::move(request.group_request()));
}

void BufferCollectionToken::V2::CreateBufferCollectionTokenGroup(
    CreateBufferCollectionTokenGroupRequest& request,
    CreateBufferCollectionTokenGroupCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollectionTokenGroup::CreateBufferCollectionTokenGroup", "this",
                 this, "logical_buffer_collection", &parent_.logical_buffer_collection());
  if (!request.group_request().has_value()) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
                     "CreateBufferCollectionTokenGroup() requires group_request set");
    return;
  }
  NodeProperties* new_node_properties;
  if (!parent_.CommonCreateBufferCollectionTokenGroupStage1(completer, &new_node_properties)) {
    return;
  }
  parent_.logical_buffer_collection().CreateBufferCollectionTokenGroupV2(
      parent_.shared_logical_buffer_collection(), new_node_properties,
      std::move(request.group_request().value()));
}

void BufferCollectionToken::V1::SetVerboseLogging(SetVerboseLoggingCompleter::Sync& completer) {
  parent_.SetVerboseLoggingImpl(completer);
}

void BufferCollectionToken::V2::SetVerboseLogging(SetVerboseLoggingCompleter::Sync& completer) {
  parent_.SetVerboseLoggingImpl(completer);
}

void BufferCollectionToken::V1::GetNodeRef(GetNodeRefCompleter::Sync& completer) {
  parent_.GetNodeRefImplV1(completer);
}

void BufferCollectionToken::V2::GetNodeRef(GetNodeRefCompleter::Sync& completer) {
  parent_.GetNodeRefImplV2(completer);
}

void BufferCollectionToken::V1::IsAlternateFor(IsAlternateForRequest& request,
                                               IsAlternateForCompleter::Sync& completer) {
  parent_.IsAlternateForImplV1(request, completer);
}

void BufferCollectionToken::V2::IsAlternateFor(IsAlternateForRequest& request,
                                               IsAlternateForCompleter::Sync& completer) {
  parent_.IsAlternateForImplV2(request, completer);
}

BufferCollectionToken::BufferCollectionToken(
    fbl::RefPtr<LogicalBufferCollection> logical_buffer_collection_param,
    NodeProperties* new_node_properties, const TokenServerEnd& server_end)
    : Node(std::move(logical_buffer_collection_param), new_node_properties,
           GetUnownedChannel(server_end)),
      LoggingMixin("BufferCollectionToken") {
  TRACE_DURATION("gfx", "BufferCollectionToken::BufferCollectionToken", "this", this,
                 "logical_buffer_collection", &logical_buffer_collection());
  inspect_node_ =
      logical_buffer_collection().inspect_node().CreateChild(CreateUniqueName("token-"));
  if (create_status() != ZX_OK) {
    // Node::Node() failed and maybe !has_server_koid().
    return;
  }
  // Node::Node filled this out (or didn't and status() reflected that, which was already checked
  // above).
  ZX_DEBUG_ASSERT(has_server_koid());
  OnServerKoid();
}

void BufferCollectionToken::FailAsync(Location location, zx_status_t status, const char* format,
                                      ...) {
  va_list args;
  va_start(args, format);
  vLog(true, location.file(), location.line(), logging_prefix(), "fail", format, args);
  va_end(args);

  // Idempotent, so only close once.
  if (!server_binding_v1_.has_value() && !server_binding_v2_.has_value()) {
    return;
  }
  ZX_DEBUG_ASSERT(!!server_binding_v1_.has_value() ^ !!server_binding_v2_.has_value());

  async_failure_result_ = status;

  if (server_binding_v1_.has_value()) {
    server_binding_v1_->Close(status);
    server_binding_v1_ = {};
  } else {
    ZX_DEBUG_ASSERT(server_binding_v2_.has_value());
    server_binding_v2_->Close(status);
    server_binding_v2_ = {};
  }
}

bool BufferCollectionToken::ReadyForAllocation() { return false; }

void BufferCollectionToken::OnBuffersAllocated(const AllocationResult& allocation_result) {
  ZX_PANIC("Unexpected call to BufferCollectionToken::OnBuffersAllocated()");
}

BufferCollectionToken* BufferCollectionToken::buffer_collection_token() { return this; }

const BufferCollectionToken* BufferCollectionToken::buffer_collection_token() const { return this; }

BufferCollection* BufferCollectionToken::buffer_collection() { return nullptr; }

const BufferCollection* BufferCollectionToken::buffer_collection() const { return nullptr; }

BufferCollectionTokenGroup* BufferCollectionToken::buffer_collection_token_group() {
  return nullptr;
}

const BufferCollectionTokenGroup* BufferCollectionToken::buffer_collection_token_group() const {
  return nullptr;
}

OrphanedNode* BufferCollectionToken::orphaned_node() { return nullptr; }

const OrphanedNode* BufferCollectionToken::orphaned_node() const { return nullptr; }

bool BufferCollectionToken::is_connected_type() const { return true; }

bool BufferCollectionToken::is_currently_connected() const {
  return server_binding_v1_.has_value() || server_binding_v2_.has_value();
}

const char* BufferCollectionToken::node_type_string() const { return "token"; }

}  // namespace sysmem_driver
