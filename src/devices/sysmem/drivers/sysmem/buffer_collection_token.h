// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_BUFFER_COLLECTION_TOKEN_H_
#define SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_BUFFER_COLLECTION_TOKEN_H_

#include <fidl/fuchsia.sysmem/cpp/wire.h>

#include "lib/zx/channel.h"
#include "logging.h"
#include "logical_buffer_collection.h"
#include "node.h"

namespace sysmem_driver {

class BufferCollectionToken;

class BufferCollectionToken : public Node, public LoggingMixin {
 public:
  ~BufferCollectionToken() override;

  // The returned reference is owned by new_node_properties, which in turn is owned by
  // logical_buffer_collection->root_.
  static BufferCollectionToken& EmplaceInTree(
      fbl::RefPtr<LogicalBufferCollection> logical_buffer_collection,
      NodeProperties* new_node_properties, const TokenServerEnd& server_end);

  template <class CompleterSync>
  void TokenCloseImpl(CompleterSync& completer) {
    // BufferCollectionToken has one additional error case we want to check, so check before calling
    // Node::CloseImpl().
    if (buffer_collection_request_) {
      FailSync(FROM_HERE, completer, ZX_ERR_BAD_STATE,
               "BufferCollectionToken::Close() when buffer_collection_request_");
      // We're failing async - no need to try to fail sync.
      return;
    }
    CloseImpl(completer);
  }

  void OnServerKoid();

  void SetDispensableInternal();

  bool is_done();

  void SetBufferCollectionRequest(CollectionServerEnd buffer_collection_request);

  std::optional<CollectionServerEnd> TakeBufferCollectionRequest();

  void CloseServerBinding(zx_status_t epitaph) override;

  // Node interface
  bool ReadyForAllocation() override;
  void OnBuffersAllocated(const AllocationResult& allocation_result) override;
  BufferCollectionToken* buffer_collection_token() override;
  const BufferCollectionToken* buffer_collection_token() const override;
  BufferCollection* buffer_collection() override;
  const BufferCollection* buffer_collection() const override;
  BufferCollectionTokenGroup* buffer_collection_token_group() override;
  const BufferCollectionTokenGroup* buffer_collection_token_group() const override;
  OrphanedNode* orphaned_node() override;
  const OrphanedNode* orphaned_node() const override;
  bool is_connected_type() const override;
  bool is_currently_connected() const override;
  const char* node_type_string() const override;

  void Bind(TokenServerEnd server_end);

 protected:
  void BindInternalV1(zx::channel token_request,
                      ErrorHandlerWrapper error_handler_wrapper) override;
  void BindInternalV2(zx::channel token_request,
                      ErrorHandlerWrapper error_handler_wrapper) override;

 private:
  friend class FidlServer;

  struct V1 : public fidl::Server<fuchsia_sysmem::BufferCollectionToken> {
    explicit V1(BufferCollectionToken& parent) : parent_(parent) {}

    // FIDL "compose Node" "interface" (identical among BufferCollection, BufferCollectionToken,
    // BufferCollectionTokenGroup)
    void Sync(SyncCompleter::Sync& completer) override;
    void DeprecatedSync(DeprecatedSyncCompleter::Sync& completer) override;
    void Close(CloseCompleter::Sync& completer) override;
    void DeprecatedClose(DeprecatedCloseCompleter::Sync& completer) override;
    void GetNodeRef(GetNodeRefCompleter::Sync& completer) override;
    void IsAlternateFor(IsAlternateForRequest& request,
                        IsAlternateForCompleter::Sync& completer) override;
    void SetName(SetNameRequest& request, SetNameCompleter::Sync& completer) override;
    void DeprecatedSetName(DeprecatedSetNameRequest& request,
                           DeprecatedSetNameCompleter::Sync& completer) override;
    void SetDebugClientInfo(SetDebugClientInfoRequest& request,
                            SetDebugClientInfoCompleter::Sync& completer) override;
    void DeprecatedSetDebugClientInfo(
        DeprecatedSetDebugClientInfoRequest& request,
        DeprecatedSetDebugClientInfoCompleter::Sync& completer) override;
    void SetDebugTimeoutLogDeadline(SetDebugTimeoutLogDeadlineRequest& request,
                                    SetDebugTimeoutLogDeadlineCompleter::Sync& completer) override;
    void DeprecatedSetDebugTimeoutLogDeadline(
        DeprecatedSetDebugTimeoutLogDeadlineRequest& request,
        DeprecatedSetDebugTimeoutLogDeadlineCompleter::Sync& completer) override;
    void SetVerboseLogging(SetVerboseLoggingCompleter::Sync& completer) override;

    //
    // fuchsia.sysmem.BufferCollectionToken interface methods (see also "compose Node" methods
    // above)
    //
    void DuplicateSync(DuplicateSyncRequest& request,
                       DuplicateSyncCompleter::Sync& completer) override;
    void Duplicate(DuplicateRequest& request, DuplicateCompleter::Sync& completer) override;
    void CreateBufferCollectionTokenGroup(
        CreateBufferCollectionTokenGroupRequest& request,
        CreateBufferCollectionTokenGroupCompleter::Sync& completer) override;
    void SetDispensable(SetDispensableCompleter::Sync& completer) override;

    BufferCollectionToken& parent_;
  };

  struct V2 : public fidl::Server<fuchsia_sysmem2::BufferCollectionToken>,
              public fbl::Recyclable<V2> {
    explicit V2(BufferCollectionToken& parent) : parent_(parent) {}

    // FIDL "compose Node" "interface" (identical among BufferCollection, BufferCollectionToken,
    // BufferCollectionTokenGroup)
    void Sync(SyncCompleter::Sync& completer) override;
    void Close(CloseCompleter::Sync& completer) override;
    void GetNodeRef(GetNodeRefCompleter::Sync& completer) override;
    void IsAlternateFor(IsAlternateForRequest& request,
                        IsAlternateForCompleter::Sync& completer) override;
    void SetName(SetNameRequest& request, SetNameCompleter::Sync& completer) override;
    void SetDebugClientInfo(SetDebugClientInfoRequest& request,
                            SetDebugClientInfoCompleter::Sync& completer) override;
    void SetDebugTimeoutLogDeadline(SetDebugTimeoutLogDeadlineRequest& request,
                                    SetDebugTimeoutLogDeadlineCompleter::Sync& completer) override;
    void SetVerboseLogging(SetVerboseLoggingCompleter::Sync& completer) override;

    //
    // fuchsia.sysmem.BufferCollectionToken interface methods (see also "compose Node" methods
    // above)
    //
    void DuplicateSync(DuplicateSyncRequest& request,
                       DuplicateSyncCompleter::Sync& completer) override;
    void Duplicate(DuplicateRequest& request, DuplicateCompleter::Sync& completer) override;
    void CreateBufferCollectionTokenGroup(
        CreateBufferCollectionTokenGroupRequest& request,
        CreateBufferCollectionTokenGroupCompleter::Sync& completer) override;
    void SetDispensable(SetDispensableCompleter::Sync& completer) override;

    BufferCollectionToken& parent_;
  };

  BufferCollectionToken(fbl::RefPtr<LogicalBufferCollection> parent,
                        NodeProperties* new_node_properties, const TokenServerEnd& server_end);

  void FailAsync(Location location, zx_status_t status, const char* format, ...);

  template <typename Completer>
  bool CommonDuplicateStage1(uint32_t rights_attenuation_mask, Completer& completer,
                             NodeProperties** out_node_properties);

  template <typename Completer>
  bool CommonCreateBufferCollectionTokenGroupStage1(Completer& completer,
                                                    NodeProperties** out_node_properties);

  std::optional<V1> v1_server_;
  std::optional<V2> v2_server_;

  std::optional<zx_status_t> async_failure_result_;

  std::optional<fidl::ServerBindingRef<fuchsia_sysmem::BufferCollectionToken>> server_binding_v1_;
  std::optional<fidl::ServerBindingRef<fuchsia_sysmem2::BufferCollectionToken>> server_binding_v2_;

  // This is set up to once during
  // LogicalBufferCollection::BindSharedCollection(), and essentially curries
  // the buffer_collection_request past the processing of any remaining
  // inbound messages on the BufferCollectionToken before starting to serve
  // the BufferCollection that the token was exchanged for.  This way, inbound
  // Duplicate() messages in the BufferCollectionToken are seen before any
  // BufferCollection::SetConstraints() (which might otherwise try to allocate
  // buffers too soon before all tokens are gone)

  std::optional<CollectionServerEnd> buffer_collection_request_;

  inspect::Node inspect_node_;
  inspect::UintProperty debug_id_property_;
  inspect::StringProperty debug_name_property_;
  inspect::ValueList properties_;
};

}  // namespace sysmem_driver

#endif  // SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_BUFFER_COLLECTION_TOKEN_H_
