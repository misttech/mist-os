// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_ALLOCATOR_H_
#define SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_ALLOCATOR_H_

#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/zx/channel.h>

#include "device.h"
#include "logging.h"
#include "logical_buffer_collection.h"

namespace sysmem_driver {

// An instance of this class serves an Allocator connection.  The lifetime of
// the instance is 1:1 with the Allocator channel.
//
// Because Allocator is essentially self-contained and handling the server end
// of a channel, most of Allocator is private.
class Allocator : public LoggingMixin {
 public:
  // Public for std::unique_ptr<Allocator>:
  ~Allocator();

  static void CreateChannelOwnedV1(zx::channel request, Device* device);
  // The returned reference lasts only until the current thread returns to the FIDL dispatcher, and
  // will likely be removed when sysmem(1) can be removed.
  static Allocator& CreateChannelOwnedV2(zx::channel request, Device* device);

 private:
  struct V1 : public fidl::Server<fuchsia_sysmem::Allocator> {
    explicit V1(std::unique_ptr<Allocator> allocator) : allocator_(std::move(allocator)) {}

    void AllocateNonSharedCollection(
        AllocateNonSharedCollectionRequest& request,
        AllocateNonSharedCollectionCompleter::Sync& completer) override;
    void AllocateSharedCollection(AllocateSharedCollectionRequest& request,
                                  AllocateSharedCollectionCompleter::Sync& completer) override;
    void BindSharedCollection(BindSharedCollectionRequest& request,
                              BindSharedCollectionCompleter::Sync& completer) override;
    void ValidateBufferCollectionToken(
        ValidateBufferCollectionTokenRequest& request,
        ValidateBufferCollectionTokenCompleter::Sync& completer) override;
    void SetDebugClientInfo(SetDebugClientInfoRequest& request,
                            SetDebugClientInfoCompleter::Sync& completer) override;
    void ConnectToSysmem2Allocator(ConnectToSysmem2AllocatorRequest& request,
                                   ConnectToSysmem2AllocatorCompleter::Sync& completer) override;

    std::unique_ptr<Allocator> allocator_;
  };

  struct V2 : public fidl::Server<fuchsia_sysmem2::Allocator> {
    explicit V2(std::unique_ptr<Allocator> allocator) : allocator_(std::move(allocator)) {}

    void AllocateNonSharedCollection(
        AllocateNonSharedCollectionRequest& request,
        AllocateNonSharedCollectionCompleter::Sync& completer) override;
    void AllocateSharedCollection(AllocateSharedCollectionRequest& request,
                                  AllocateSharedCollectionCompleter::Sync& completer) override;
    void BindSharedCollection(BindSharedCollectionRequest& request,
                              BindSharedCollectionCompleter::Sync& completer) override;
    void ValidateBufferCollectionToken(
        ValidateBufferCollectionTokenRequest& request,
        ValidateBufferCollectionTokenCompleter::Sync& completer) override;
    void SetDebugClientInfo(SetDebugClientInfoRequest& request,
                            SetDebugClientInfoCompleter::Sync& completer) override;
    void GetVmoInfo(GetVmoInfoRequest& request, GetVmoInfoCompleter::Sync& completer) override;

    void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_sysmem2::Allocator> metadata,
                               fidl::UnknownMethodCompleter::Sync& completer) override;

    std::unique_ptr<Allocator> allocator_;
  };

  Allocator(Device* parent_device);

  template <typename Completer, typename Protocol>
  fit::result<std::monostate, fidl::Endpoints<Protocol>> CommonAllocateNonSharedCollection(
      Completer& completer);

  Device* parent_device_ = nullptr;

  std::optional<ClientDebugInfo> client_debug_info_;
};

}  // namespace sysmem_driver

#endif  // SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_ALLOCATOR_H_
