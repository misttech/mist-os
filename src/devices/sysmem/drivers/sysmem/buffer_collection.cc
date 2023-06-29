// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "buffer_collection.h"

#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/common_types.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/wire_types.h>
#include <lib/ddk/trace/event.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <lib/fidl/cpp/wire_natural_conversions.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zx/channel.h>
#include <lib/zx/object.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <atomic>

#include <fbl/ref_ptr.h>

#include "buffer_collection_token.h"
#include "logical_buffer_collection.h"
#include "node_properties.h"
#include "utils.h"

namespace sysmem_driver {

namespace {

// For max client vmo rights, we specify the RIGHT bits individually to avoid
// picking up any newly-added rights unintentionally.  This is based on
// ZX_DEFAULT_VMO_RIGHTS, but with a few rights removed.
const uint32_t kMaxClientVmoRights =
    // ZX_RIGHTS_BASIC, except ZX_RIGHT_INSPECT (at least for now).
    ZX_RIGHT_TRANSFER | ZX_RIGHT_DUPLICATE | ZX_RIGHT_WAIT |
    // ZX_RIGHTS_IO:
    ZX_RIGHT_READ | ZX_RIGHT_WRITE |
    // ZX_RIGHTS_PROPERTY allows a participant to set ZX_PROP_NAME for easier
    // memory metrics.  Nothing prevents participants from figting over the
    // name, though the kernel should make each set/get atomic with respect to
    // other set/get.  This relies on ZX_RIGHTS_PROPERTY not implying anything
    // that could be used as an attack vector between processes sharing a VMO.
    ZX_RIGHTS_PROPERTY |
    // We intentionally omit ZX_RIGHT_EXECUTE (indefinitely) and ZX_RIGHT_SIGNAL
    // (at least for now).
    //
    // Remaining bits of ZX_DEFAULT_VMO_RIGHTS (as of this writing):
    ZX_RIGHT_MAP;

}  // namespace

// static
BufferCollection& BufferCollection::EmplaceInTree(
    fbl::RefPtr<LogicalBufferCollection> logical_buffer_collection, BufferCollectionToken* token,
    const CollectionServerEnd& server_end) {
  // The token is passed in as a pointer because this method deletes token, but the caller must
  // provide non-nullptr token.
  ZX_DEBUG_ASSERT(token);
  // This conversion from unique_ptr<> to RefPtr<> will go away once we move BufferCollection to
  // LLCPP FIDL server binding.
  fbl::RefPtr<Node> node(
      fbl::AdoptRef(new BufferCollection(logical_buffer_collection, *token, server_end)));
  BufferCollection* buffer_collection_ptr = static_down_cast<BufferCollection*>(node.get());
  // This also deletes token.
  token->node_properties().SetNode(std::move(node));
  return *buffer_collection_ptr;
}

BufferCollection::~BufferCollection() {
  TRACE_DURATION("gfx", "BufferCollection::~BufferCollection", "this", this,
                 "logical_buffer_collection", &logical_buffer_collection());
}

void BufferCollection::CloseServerBinding(zx_status_t epitaph) {
  if (server_binding_v1_.has_value()) {
    server_binding_v1_->Close(epitaph);
  }
  if (server_binding_v2_.has_value()) {
    server_binding_v2_->Close(epitaph);
  }
  server_binding_v1_ = {};
  server_binding_v2_ = {};
}

void BufferCollection::Bind(CollectionServerEnd collection_server_end) {
  Node::Bind(TakeNodeServerEnd(std::move(collection_server_end)));
}

void BufferCollection::BindInternalV1(zx::channel collection_request,
                                      ErrorHandlerWrapper error_handler_wrapper) {
  v1_server_.emplace(*this);
  server_binding_v1_ = fidl::BindServer(
      parent_device()->dispatcher(),
      fidl::ServerEnd<fuchsia_sysmem::BufferCollection>(std::move(collection_request)),
      &v1_server_.value(),
      [error_handler_wrapper = std::move(error_handler_wrapper)](
          BufferCollection::V1* collection, fidl::UnbindInfo info, CollectionServerEndV1 channel) {
        error_handler_wrapper(info);
      });
}

void BufferCollection::BindInternalV2(zx::channel collection_request,
                                      ErrorHandlerWrapper error_handler_wrapper) {
  v2_server_.emplace(*this);
  server_binding_v2_ = fidl::BindServer(
      parent_device()->dispatcher(),
      fidl::ServerEnd<fuchsia_sysmem2::BufferCollection>(std::move(collection_request)),
      &v2_server_.value(),
      [error_handler_wrapper = std::move(error_handler_wrapper)](
          BufferCollection::V2* collection, fidl::UnbindInfo info, CollectionServerEndV2 channel) {
        error_handler_wrapper(info);
      });
}

void BufferCollection::BindInternalCombinedV1AndV2(zx::channel server_end,
                                                   ErrorHandlerWrapper error_handler_wrapper) {
  ZX_PANIC("BufferCollection only serves V1 or V2 separately - never combined V1 and V2");
}

void BufferCollection::V1::Sync(SyncCompleter::Sync& completer) { parent_.SyncImpl(completer); }

void BufferCollection::V2::Sync(SyncCompleter::Sync& completer) { parent_.SyncImpl(completer); }

void BufferCollection::V1::DeprecatedSync(DeprecatedSyncCompleter::Sync& completer) {
  parent_.SyncImpl(completer);
}

void BufferCollection::V1::SetConstraintsAuxBuffers(
    SetConstraintsAuxBuffersRequest& request, SetConstraintsAuxBuffersCompleter::Sync& completer) {
  if (parent_.is_set_constraints_aux_buffers_seen_) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_NOT_SUPPORTED,
                     "SetConstraintsAuxBuffers() can be called only once.");
    return;
  }
  parent_.is_set_constraints_aux_buffers_seen_ = true;
  if (parent_.is_done_) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
                     "BufferCollectionToken::SetConstraintsAuxBuffers() when already is_done_");
    return;
  }
  if (parent_.is_set_constraints_seen_) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_NOT_SUPPORTED,
                     "SetConstraintsAuxBuffers() after SetConstraints() causes failure.");
    return;
  }
  ZX_DEBUG_ASSERT(!parent_.constraints_aux_buffers_.has_value());
  parent_.constraints_aux_buffers_.emplace(std::move(request.constraints()));
  // LogicalBufferCollection doesn't care about "clear aux buffers" constraints until the last
  // SetConstraints(), so done for now.
}

template <typename Completer>
bool BufferCollection::CommonSetConstraintsStage1(Completer& completer) {
  if (is_set_constraints_seen_) {
    FailSync(FROM_HERE, completer, ZX_ERR_NOT_SUPPORTED, "2nd SetConstraints() causes failure.");
    return false;
  }
  is_set_constraints_seen_ = true;
  if (is_done_) {
    FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE, "SetConstraints() when already is_done_");
    // We're failing async - no need to try to fail sync.
    return false;
  }
  return true;
}

void BufferCollection::V1::SetConstraints(SetConstraintsRequest& request,
                                          SetConstraintsCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollection::V1::SetConstraints", "this", this,
                 "logical_buffer_collection", &parent_.logical_buffer_collection());
  std::optional<fuchsia_sysmem::BufferCollectionConstraints> local_constraints(
      std::move(request.constraints()));

  if (!parent_.CommonSetConstraintsStage1(completer)) {
    return;
  }

  if (!request.has_constraints()) {
    // Not needed.
    local_constraints.reset();
    if (parent_.is_set_constraints_aux_buffers_seen_) {
      // No constraints are fine, just not aux buffers constraints without main constraints, because
      // I can't think of any reason why we'd need to support aux buffers constraints without main
      // constraints, so disallow at least for now.
      parent_.FailSync(FROM_HERE, completer, ZX_ERR_NOT_SUPPORTED,
                       "SetConstraintsAuxBuffers() && !has_constraints");
      return;
    }
  }

  ZX_DEBUG_ASSERT(!parent_.has_constraints());
  // enforced above
  ZX_DEBUG_ASSERT(!parent_.constraints_aux_buffers_.has_value() || local_constraints.has_value());
  ZX_DEBUG_ASSERT(request.has_constraints() == local_constraints.has_value());

  {  // scope result
    auto result = sysmem::V2CopyFromV1BufferCollectionConstraints(
        local_constraints.has_value() ? &local_constraints.value() : nullptr,
        parent_.constraints_aux_buffers_.has_value() ? &(*parent_.constraints_aux_buffers_)
                                                     : nullptr);
    if (!result.is_ok()) {
      parent_.FailSync(FROM_HERE, completer, ZX_ERR_INVALID_ARGS,
                       "V2CopyFromV1BufferCollectionConstraints() failed");
      return;
    }
    ZX_DEBUG_ASSERT(!result.value().IsEmpty() || !local_constraints.has_value());
    parent_.node_properties().SetBufferCollectionConstraints(result.take_value());
  }  // ~result

  // No longer needed.
  parent_.constraints_aux_buffers_.reset();

  parent_.node_properties().LogInfo(FROM_HERE, "BufferCollection::V1::SetConstraints()");
  parent_.node_properties().LogConstraints(FROM_HERE);

  // LogicalBufferCollection will ask for constraints when it needs them,
  // possibly during this call if this is the last participant to report
  // having initial constraints.
  //
  // The LogicalBufferCollection cares if this BufferCollection view has null
  // constraints, but only later when it asks for the specific constraints.
  parent_.logical_buffer_collection().OnNodeReady();
  // |this| may be gone at this point, if the allocation failed.  Regardless,
  // SetConstraints() worked, so ZX_OK.
}

void BufferCollection::V2::SetConstraints(SetConstraintsRequest& request,
                                          SetConstraintsCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollection::V2::SetConstraints", "this", this,
                 "logical_buffer_collection", &parent_.logical_buffer_collection());

  if (!parent_.CommonSetConstraintsStage1(completer)) {
    return;
  }

  ZX_DEBUG_ASSERT(!parent_.has_constraints());

  // Normalize non-set constraints to constraints with every field non-set.  These are semantically
  // equivalent.
  fuchsia_sysmem2::BufferCollectionConstraints local_constraints;
  if (request.constraints().has_value()) {
    local_constraints = std::move(request.constraints().value());
  }

  parent_.node_properties().SetBufferCollectionConstraints(std::move(local_constraints));

  parent_.node_properties().LogInfo(FROM_HERE, "BufferCollection::V2::SetConstraints()");
  parent_.node_properties().LogConstraints(FROM_HERE);

  // LogicalBufferCollection will ask for constraints when it needs them,
  // possibly during this call if this is the last participant to report
  // having initial constraints.
  //
  // The LogicalBufferCollection cares if this BufferCollection view has null
  // constraints, but only later when it asks for the specific constraints.
  parent_.logical_buffer_collection().OnNodeReady();
  // |this| may be gone at this point, if the allocation failed.  Regardless,
  // SetConstraints() worked, so ZX_OK.
}

template <typename Completer>
bool BufferCollection::CommonWaitForAllBuffersAllocatedStage1(Completer& completer,
                                                              trace_async_id_t* out_event_id) {
  if (is_done_) {
    FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
             "BufferCollection::WaitForAllBuffersAllocated() when already is_done_");
    return false;
  }
  trace_async_id_t current_event_id = TRACE_NONCE();
  TRACE_ASYNC_BEGIN("gfx", "BufferCollection::WaitForAllBuffersAllocated async", current_event_id,
                    "this", this, "logical_buffer_collection", &logical_buffer_collection());
  *out_event_id = current_event_id;
  return true;
}

void BufferCollection::V1::WaitForBuffersAllocated(
    WaitForBuffersAllocatedCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollection::V1::WaitForBuffersAllocated", "this", this,
                 "logical_buffer_collection", &parent_.logical_buffer_collection());

  trace_async_id_t current_event_id;
  if (!parent_.CommonWaitForAllBuffersAllocatedStage1(completer, &current_event_id)) {
    return;
  }

  parent_.pending_wait_for_buffers_allocated_v1_.emplace_back(
      std::make_pair(current_event_id, completer.ToAsync()));
  // The allocation is a one-shot (once true, remains true) and may already be done, in which case
  // this immediately completes txn.
  parent_.MaybeCompleteWaitForBuffersAllocated();
}

void BufferCollection::V2::WaitForAllBuffersAllocated(
    WaitForAllBuffersAllocatedCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollection::V2::WaitForAllBuffersAllocated", "this", this,
                 "logical_buffer_collection", &parent_.logical_buffer_collection());

  trace_async_id_t current_event_id = TRACE_NONCE();
  if (!parent_.CommonWaitForAllBuffersAllocatedStage1(completer, &current_event_id)) {
    return;
  }

  parent_.pending_wait_for_buffers_allocated_v2_.emplace_back(
      std::make_pair(current_event_id, completer.ToAsync()));
  // The allocation is a one-shot (once true, remains true) and may already be done, in which case
  // this immediately completes txn.
  parent_.MaybeCompleteWaitForBuffersAllocated();
}

template <typename Completer>
bool BufferCollection::CommonCheckAllBuffersAllocatedStage1(Completer& completer,
                                                            zx_status_t* result) {
  if (is_done_) {
    FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
             "BufferCollectionToken::V1::CheckBuffersAllocated() when "
             "already is_done_");
    // We're failing async - no need to try to fail sync.
    return false;
  }
  if (!logical_allocation_result_.has_value()) {
    *result = ZX_ERR_UNAVAILABLE;
    return true;
  }
  // Buffer collection has either been allocated or failed.
  *result = logical_allocation_result_->status;
  return true;
}

void BufferCollection::V1::CheckBuffersAllocated(CheckBuffersAllocatedCompleter::Sync& completer) {
  zx_status_t result;
  if (!parent_.CommonCheckAllBuffersAllocatedStage1(completer, &result)) {
    return;
  }
  completer.Reply(result);
}

void BufferCollection::V2::CheckAllBuffersAllocated(
    CheckAllBuffersAllocatedCompleter::Sync& completer) {
  zx_status_t result;
  if (!parent_.CommonCheckAllBuffersAllocatedStage1(completer, &result)) {
    return;
  }
  fuchsia_sysmem2::BufferCollectionCheckAllBuffersAllocatedResponse response;
  if (result == ZX_OK) {
    completer.Reply(fit::ok(response));
  } else {
    completer.Reply(fit::error(result));
  }
}

void BufferCollection::V1::GetAuxBuffers(GetAuxBuffersCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollection::GetAuxBuffers", "this", this,
                 "logical_buffer_collection", &parent_.logical_buffer_collection());
  if (!parent_.logical_allocation_result_.has_value()) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
                     "GetAuxBuffers() called before allocation complete.");
    return;
  }
  if (parent_.logical_allocation_result_->status != ZX_OK) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
                     "GetAuxBuffers() called after allocation failure.");
    // We're failing async - no need to fail sync.
    return;
  }
  ZX_DEBUG_ASSERT(parent_.logical_allocation_result_->buffer_collection_info);
  auto v1_result = parent_.CloneAuxBuffersResultForSendingV1(
      *parent_.logical_allocation_result_->buffer_collection_info);
  if (!v1_result.is_ok()) {
    // Close to avoid assert.
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_INTERNAL,
                     "CloneAuxBuffersResultForSendingV1() failed.");
    return;
  }
  auto v1 = v1_result.take_value();
  fuchsia_sysmem::BufferCollectionGetAuxBuffersResponse response;
  response.status() = parent_.logical_allocation_result_->status;
  response.buffer_collection_info_aux_buffers() = std::move(v1);
  completer.Reply(std::move(response));
}

template <typename Completer>
bool BufferCollection::CommonAttachTokenStage1(uint32_t rights_attenuation_mask,
                                               Completer& completer,
                                               NodeProperties** out_node_properties) {
  if (is_done_) {
    // This is Close() followed by AttachToken(), which is not permitted and causes the
    // BufferCollection to fail.
    FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
             "BufferCollection::AttachToken() attempted when is_done_");
    return false;
  }

  if (rights_attenuation_mask == 0) {
    FailSync(FROM_HERE, completer, ZX_ERR_INVALID_ARGS,
             "rights_attenuation_mask of 0 is forbidden");
    return false;
  }

  NodeProperties* new_node_properties = node_properties().NewChild(&logical_buffer_collection());

  // These defaults can be overriden by Allocator.SetDebugClientInfo() called before
  // BindSharedCollection().
  if (!new_node_properties->client_debug_info().name.empty()) {
    // This can be overriden via Allocator::SetDebugClientInfo(), but in case that's not called,
    // this default may help clarify where the new token / BufferCollection channel came from.
    new_node_properties->client_debug_info().name =
        new_node_properties->client_debug_info().name + " then AttachToken()";
  } else {
    new_node_properties->client_debug_info().name = "from AttachToken()";
  }

  if (rights_attenuation_mask != ZX_RIGHT_SAME_RIGHTS) {
    new_node_properties->rights_attenuation_mask() &= rights_attenuation_mask;
  }

  // All AttachToken() tokesn are ErrorPropagationMode::kDoNotPropagate from the start.
  new_node_properties->error_propagation_mode() = ErrorPropagationMode::kDoNotPropagate;

  *out_node_properties = new_node_properties;

  return true;
}

void BufferCollection::V1::AttachToken(AttachTokenRequest& request,
                                       AttachTokenCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollection::V1::AttachToken", "this", this,
                 "logical_buffer_collection", &parent_.logical_buffer_collection());

  NodeProperties* new_node_properties;
  if (!parent_.CommonAttachTokenStage1(request.rights_attenuation_mask(), completer,
                                       &new_node_properties)) {
    return;
  }

  parent_.logical_buffer_collection().CreateBufferCollectionTokenV1(
      parent_.shared_logical_buffer_collection(), new_node_properties,
      std::move(request.token_request()));
}

void BufferCollection::V2::AttachToken(AttachTokenRequest& request,
                                       AttachTokenCompleter::Sync& completer) {
  TRACE_DURATION("gfx", "BufferCollection::V2::AttachToken", "this", this,
                 "logical_buffer_collection", &parent_.logical_buffer_collection());

  if (!request.rights_attenuation_mask().has_value()) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_INVALID_ARGS,
                     "V2::AttachToken() requires rights_attenuation_mask set");
    return;
  }

  if (!request.token_request().has_value()) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_INVALID_ARGS,
                     "V2::AttachToken() requires token_request set");
    return;
  }

  NodeProperties* new_node_properties;
  if (!parent_.CommonAttachTokenStage1(request.rights_attenuation_mask().value(), completer,
                                       &new_node_properties)) {
    return;
  }

  parent_.logical_buffer_collection().CreateBufferCollectionTokenV2(
      parent_.shared_logical_buffer_collection(), new_node_properties,
      std::move(request.token_request().value()));
}

template <typename Completer>
void BufferCollection::CommonAttachLifetimeTracking(zx::eventpair server_end,
                                                    uint32_t buffers_remaining,
                                                    Completer& completer) {
  TRACE_DURATION("gfx", "BufferCollection::AttachLifetimeTracking", "this", this,
                 "logical_buffer_collection", &logical_buffer_collection());
  if (is_done_) {
    // This is Close() followed by AttachLifetimeTracking() which is not permitted and causes the
    // BufferCollection to fail.
    FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
             "BufferCollection::AttachLifetimeTracking() attempted when is_done_");
    return;
  }
  pending_lifetime_tracking_.emplace_back(PendingLifetimeTracking{
      .server_end = std::move(server_end), .buffers_remaining = buffers_remaining});
  MaybeFlushPendingLifetimeTracking();
}

void BufferCollection::V1::AttachLifetimeTracking(
    AttachLifetimeTrackingRequest& request, AttachLifetimeTrackingCompleter::Sync& completer) {
  parent_.CommonAttachLifetimeTracking(std::move(request.server_end()), request.buffers_remaining(),
                                       completer);
}

void BufferCollection::V2::AttachLifetimeTracking(
    AttachLifetimeTrackingRequest& request, AttachLifetimeTrackingCompleter::Sync& completer) {
  if (!request.server_end().has_value()) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
                     "AttachLifetimeTracking() requires server_end set");
    return;
  }
  if (!request.buffers_remaining().has_value()) {
    parent_.FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
                     "AttachLifetimeTracking() requires buffers_remaining set");
    return;
  }
  parent_.CommonAttachLifetimeTracking(std::move(request.server_end().value()),
                                       request.buffers_remaining().value(), completer);
}

void BufferCollection::V1::SetVerboseLogging(SetVerboseLoggingCompleter::Sync& completer) {
  parent_.SetVerboseLoggingImpl(completer);
}

void BufferCollection::V2::SetVerboseLogging(SetVerboseLoggingCompleter::Sync& completer) {
  parent_.SetVerboseLoggingImpl(completer);
}

void BufferCollection::V1::GetNodeRef(GetNodeRefCompleter::Sync& completer) {
  parent_.GetNodeRefImplV1(completer);
}

void BufferCollection::V2::GetNodeRef(GetNodeRefCompleter::Sync& completer) {
  parent_.GetNodeRefImplV2(completer);
}

void BufferCollection::V1::IsAlternateFor(IsAlternateForRequest& request,
                                          IsAlternateForCompleter::Sync& completer) {
  parent_.IsAlternateForImplV1(request, completer);
}

void BufferCollection::V2::IsAlternateFor(IsAlternateForRequest& request,
                                          IsAlternateForCompleter::Sync& completer) {
  parent_.IsAlternateForImplV2(request, completer);
}

void BufferCollection::V1::Close(CloseCompleter::Sync& completer) { parent_.CloseImpl(completer); }

void BufferCollection::V2::Close(CloseCompleter::Sync& completer) { parent_.CloseImpl(completer); }

void BufferCollection::V1::DeprecatedClose(DeprecatedCloseCompleter::Sync& completer) {
  parent_.CloseImpl(completer);
}

void BufferCollection::V1::SetName(SetNameRequest& request, SetNameCompleter::Sync& completer) {
  parent_.SetNameImplV1(request, completer);
}

void BufferCollection::V2::SetName(SetNameRequest& request, SetNameCompleter::Sync& completer) {
  parent_.SetNameImplV2(request, completer);
}

void BufferCollection::V1::DeprecatedSetName(DeprecatedSetNameRequest& request,
                                             DeprecatedSetNameCompleter::Sync& completer) {
  parent_.SetNameImplV1(request, completer);
}

void BufferCollection::V1::SetDebugClientInfo(SetDebugClientInfoRequest& request,
                                              SetDebugClientInfoCompleter::Sync& completer) {
  parent_.SetDebugClientInfoImplV1(request, completer);
}

void BufferCollection::V2::SetDebugClientInfo(SetDebugClientInfoRequest& request,
                                              SetDebugClientInfoCompleter::Sync& completer) {
  parent_.SetDebugClientInfoImplV2(request, completer);
}

void BufferCollection::V1::DeprecatedSetDebugClientInfo(
    DeprecatedSetDebugClientInfoRequest& request,
    DeprecatedSetDebugClientInfoCompleter::Sync& completer) {
  parent_.SetDebugClientInfoImplV1(request, completer);
}

void BufferCollection::V1::SetDebugTimeoutLogDeadline(
    SetDebugTimeoutLogDeadlineRequest& request,
    SetDebugTimeoutLogDeadlineCompleter::Sync& completer) {
  parent_.SetDebugTimeoutLogDeadlineImplV1(request, completer);
}

void BufferCollection::V2::SetDebugTimeoutLogDeadline(
    SetDebugTimeoutLogDeadlineRequest& request,
    SetDebugTimeoutLogDeadlineCompleter::Sync& completer) {
  parent_.SetDebugTimeoutLogDeadlineImplV2(request, completer);
}

void BufferCollection::FailAsync(Location location, zx_status_t status, const char* format, ...) {
  va_list args;
  va_start(args, format);
  logical_buffer_collection().VLogClientError(location, &node_properties(), format, args);
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

template <typename Completer>
void BufferCollection::FailSync(Location location, Completer& completer, zx_status_t status,
                                const char* format, ...) {
  va_list args;
  va_start(args, format);
  logical_buffer_collection().VLogClientError(location, &node_properties(), format, args);
  va_end(args);

  completer.Close(status);
  async_failure_result_ = status;
}

fpromise::result<fuchsia_sysmem2::BufferCollectionInfo> BufferCollection::CloneResultForSendingV2(
    const fuchsia_sysmem2::BufferCollectionInfo& buffer_collection_info) {
  auto clone_result = sysmem::V2CloneBufferCollectionInfo(
      buffer_collection_info, GetClientVmoRights(), GetClientAuxVmoRights());
  if (!clone_result.is_ok()) {
    FailAsync(FROM_HERE, clone_result.error(),
              "CloneResultForSendingV1() V2CloneBufferCollectionInfo() failed - status: %d",
              clone_result.error());
    return fpromise::error();
  }
  auto v2_b = clone_result.take_value();
  ZX_DEBUG_ASSERT(has_constraints());

  if (!constraints().usage().has_value() || !IsAnyUsage(constraints().usage().value())) {
    // No VMO handles should be sent to the client in this case.
    if (v2_b.buffers().has_value()) {
      for (auto& vmo_buffer : v2_b.buffers().value()) {
        if (vmo_buffer.vmo().has_value()) {
          vmo_buffer.vmo().reset();
        }
        if (vmo_buffer.aux_vmo().has_value()) {
          vmo_buffer.aux_vmo().reset();
        }
      }
    }
  }

  return fpromise::ok(std::move(v2_b));
}

fpromise::result<fuchsia_sysmem::BufferCollectionInfo2> BufferCollection::CloneResultForSendingV1(
    const fuchsia_sysmem2::BufferCollectionInfo& buffer_collection_info) {
  auto v2_result = CloneResultForSendingV2(buffer_collection_info);
  if (!v2_result.is_ok()) {
    // FailAsync() already called.
    return fpromise::error();
  }
  auto v1_result = sysmem::V1MoveFromV2BufferCollectionInfo(v2_result.take_value());
  if (!v1_result.is_ok()) {
    FailAsync(FROM_HERE, ZX_ERR_INVALID_ARGS,
              "CloneResultForSendingV1() V1MoveFromV2BufferCollectionInfo() failed");
    return fpromise::error();
  }
  return v1_result;
}

fpromise::result<fuchsia_sysmem::BufferCollectionInfo2>
BufferCollection::CloneAuxBuffersResultForSendingV1(
    const fuchsia_sysmem2::BufferCollectionInfo& buffer_collection_info) {
  auto v2_result = CloneResultForSendingV2(buffer_collection_info);
  if (!v2_result.is_ok()) {
    // FailAsync() already called.
    return fpromise::error();
  }
  auto v1_result = sysmem::V1AuxBuffersMoveFromV2BufferCollectionInfo(v2_result.take_value());
  if (!v1_result.is_ok()) {
    FailAsync(FROM_HERE, ZX_ERR_INVALID_ARGS,
              "CloneResultForSendingV1() V1MoveFromV2BufferCollectionInfo() failed");
    return fpromise::error();
  }
  return fpromise::ok(v1_result.take_value());
}

void BufferCollection::OnBuffersAllocated(const AllocationResult& allocation_result) {
  TRACE_DURATION("gfx", "BufferCollection::OnBuffersAllocated", "status", allocation_result.status);
  ZX_DEBUG_ASSERT(!logical_allocation_result_.has_value());

  ZX_DEBUG_ASSERT((allocation_result.status == ZX_OK) ==
                  !!allocation_result.buffer_collection_info);

  node_properties().SetBuffersLogicallyAllocated();

  logical_allocation_result_.emplace(allocation_result);

  // Any that are pending are completed by this call or something called
  // FailAsync().  It's fine for this method to ignore the fact that
  // FailAsync() may have already been called.  That's essentially the main
  // reason we have FailAsync() instead of Fail().
  MaybeCompleteWaitForBuffersAllocated();

  // If there are any, they'll be flushed to LogicalBufferCollection now.
  MaybeFlushPendingLifetimeTracking();
}

bool BufferCollection::has_constraints() { return node_properties().has_constraints(); }

const fuchsia_sysmem2::BufferCollectionConstraints& BufferCollection::constraints() {
  ZX_DEBUG_ASSERT(has_constraints());
  return *node_properties().buffer_collection_constraints();
}

fuchsia_sysmem2::BufferCollectionConstraints BufferCollection::CloneConstraints() {
  ZX_DEBUG_ASSERT(has_constraints());
  // copy / clone
  return constraints();
}

BufferCollection::BufferCollection(
    fbl::RefPtr<LogicalBufferCollection> logical_buffer_collection_param,
    const BufferCollectionToken& token, const CollectionServerEnd& server_end)
    : Node(std::move(logical_buffer_collection_param), &token.node_properties(),
           GetUnownedChannel(server_end)) {
  TRACE_DURATION("gfx", "BufferCollection::BufferCollection", "this", this,
                 "logical_buffer_collection", &this->logical_buffer_collection());
  ZX_DEBUG_ASSERT(shared_logical_buffer_collection());
  inspect_node_ =
      this->logical_buffer_collection().inspect_node().CreateChild(CreateUniqueName("collection-"));
}

// This method is only meant to be called from GetClientVmoRights().
uint32_t BufferCollection::GetUsageBasedRightsAttenuation() {
  // This method won't be called for participants without constraints.
  ZX_DEBUG_ASSERT(has_constraints());

  // We assume that read and map are both needed by all participants with any "usage".  Only
  // ZX_RIGHT_WRITE is controlled by usage.

  // It's not this method's job to attenuate down to kMaxClientVmoRights, so
  // let's not pretend like it is.
  uint32_t result = std::numeric_limits<uint32_t>::max();
  if (!constraints().usage().has_value() || !IsWriteUsage(*constraints().usage())) {
    result &= ~ZX_RIGHT_WRITE;
  }

  return result;
}

uint32_t BufferCollection::GetClientVmoRights() {
  return
      // max possible rights for a client to have
      kMaxClientVmoRights &
      // attenuate write if client doesn't need write
      GetUsageBasedRightsAttenuation() &
      // attenuate according to BufferCollectionToken.Duplicate() rights
      // parameter so that initiator and participant can distribute the token
      // and remove any unnecessary/unintended rights along the way.
      node_properties().rights_attenuation_mask();
}

uint32_t BufferCollection::GetClientAuxVmoRights() {
  // At least for now.
  return GetClientVmoRights();
}

void BufferCollection::MaybeCompleteWaitForBuffersAllocated() {
  if (!logical_allocation_result_.has_value()) {
    // Everything is ok so far, but allocation isn't done yet.
    return;
  }
  while (!pending_wait_for_buffers_allocated_v1_.empty()) {
    auto [async_id, txn] = std::move(pending_wait_for_buffers_allocated_v1_.front());
    pending_wait_for_buffers_allocated_v1_.pop_front();

    fuchsia_sysmem::BufferCollectionInfo2 v1;
    if (logical_allocation_result_->status == ZX_OK) {
      ZX_DEBUG_ASSERT(logical_allocation_result_->buffer_collection_info);
      auto v1_result = CloneResultForSendingV1(*logical_allocation_result_->buffer_collection_info);
      if (!v1_result.is_ok()) {
        // FailAsync() already called.
        return;
      }
      v1 = v1_result.take_value();
    }
    TRACE_ASYNC_END("gfx", "BufferCollection::WaitForAllBuffersAllocated async", async_id, "this",
                    this, "logical_buffer_collection", &logical_buffer_collection());

    fuchsia_sysmem::BufferCollectionWaitForBuffersAllocatedResponse response;
    response.status() = logical_allocation_result_->status;
    response.buffer_collection_info() = std::move(v1);
    txn.Reply(std::move(response));

    fidl::Status reply_status = txn.result_of_reply();
    if (!reply_status.ok()) {
      FailAsync(FROM_HERE, reply_status.status(),
                "fuchsia_sysmem_BufferCollectionWaitForBuffersAllocated_"
                "reply failed - status: %s",
                reply_status.FormatDescription().c_str());
      return;
    }
    // ~txn
  }
  while (!pending_wait_for_buffers_allocated_v2_.empty()) {
    auto [async_id, txn] = std::move(pending_wait_for_buffers_allocated_v2_.front());
    pending_wait_for_buffers_allocated_v2_.pop_front();

    fuchsia_sysmem2::BufferCollectionInfo v2;
    if (logical_allocation_result_->status == ZX_OK) {
      ZX_DEBUG_ASSERT(logical_allocation_result_->buffer_collection_info);
      auto v2_result = CloneResultForSendingV2(*logical_allocation_result_->buffer_collection_info);
      if (!v2_result.is_ok()) {
        // FailAsync() already called.
        return;
      }
      v2 = v2_result.take_value();
    }
    TRACE_ASYNC_END("gfx", "BufferCollection::WaitForAllBuffersAllocated async", async_id, "this",
                    this, "logical_buffer_collection", &logical_buffer_collection());
    if (logical_allocation_result_->status != ZX_OK) {
      txn.Reply(fit::error(logical_allocation_result_->status));
    } else {
      fuchsia_sysmem2::BufferCollectionWaitForAllBuffersAllocatedResponse response;
      response.buffer_collection_info() = std::move(v2);
      txn.Reply(fit::ok(std::move(response)));
    }
    fidl::Status reply_status = txn.result_of_reply();
    if (!reply_status.ok()) {
      FailAsync(FROM_HERE, reply_status.status(),
                "fuchsia_sysmem2::BufferCollectionWaitForBuffersAllocated "
                "reply failed - status: %s",
                reply_status.FormatDescription().c_str());
      return;
    }
    // ~txn
  }
}

void BufferCollection::MaybeFlushPendingLifetimeTracking() {
  if (!node_properties().buffers_logically_allocated()) {
    return;
  }
  if (logical_allocation_result_->status != ZX_OK) {
    // We close these immediately if logical allocation failed, regardless of
    // the number of buffers potentially allocated in the overall
    // LogicalBufferCollection.  This is for behavior consistency between
    // AttachToken() sub-trees and the root_.
    pending_lifetime_tracking_.clear();
    return;
  }
  for (auto& pending : pending_lifetime_tracking_) {
    logical_buffer_collection().AttachLifetimeTracking(std::move(pending.server_end),
                                                       pending.buffers_remaining);
  }
  pending_lifetime_tracking_.clear();
}

bool BufferCollection::ReadyForAllocation() { return has_constraints(); }

BufferCollectionToken* BufferCollection::buffer_collection_token() { return nullptr; }

const BufferCollectionToken* BufferCollection::buffer_collection_token() const { return nullptr; }

BufferCollection* BufferCollection::buffer_collection() { return this; }

const BufferCollection* BufferCollection::buffer_collection() const { return this; }

BufferCollectionTokenGroup* BufferCollection::buffer_collection_token_group() { return nullptr; }

const BufferCollectionTokenGroup* BufferCollection::buffer_collection_token_group() const {
  return nullptr;
}

OrphanedNode* BufferCollection::orphaned_node() { return nullptr; }

const OrphanedNode* BufferCollection::orphaned_node() const { return nullptr; }

bool BufferCollection::is_connected_type() const { return true; }

bool BufferCollection::is_currently_connected() const {
  return server_binding_v1_.has_value() || server_binding_v2_.has_value();
}

const char* BufferCollection::node_type_string() const { return "collection"; }

}  // namespace sysmem_driver
