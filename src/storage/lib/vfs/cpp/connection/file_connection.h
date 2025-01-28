// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_CONNECTION_FILE_CONNECTION_H_
#define SRC_STORAGE_LIB_VFS_CPP_CONNECTION_FILE_CONNECTION_H_

#ifndef __Fuchsia__
#error "Fuchsia-only header"
#endif

#include <fidl/fuchsia.io/cpp/wire.h>
#include <zircon/availability.h>

#include <cstdint>

#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/connection/connection.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fs::internal {

class FileConnection : public Connection, public fidl::WireServer<fuchsia_io::File> {
 public:
  // Refer to documentation for |Connection::Connection|.
  FileConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode, fuchsia_io::Rights rights,
                 zx_koid_t koid);

 protected:
  virtual const zx::stream* stream() const { return nullptr; }

  virtual bool GetAppend() const = 0;
  virtual zx::result<> SetAppend(bool append) = 0;

  //
  // |fs::Connection| Implementation
  //

  void BindImpl(zx::channel channel, OnUnbound on_unbound) final;
  void Unbind() final;
  zx::result<> WithRepresentation(
      fit::callback<zx::result<>(fuchsia_io::wire::Representation)> handler,
      std::optional<fuchsia_io::NodeAttributesQuery> query) const final;
  zx_status_t WithNodeInfoDeprecated(
      fit::callback<zx_status_t(fuchsia_io::wire::NodeInfoDeprecated)> handler) const final;

  //
  // |fuchsia.io/Node| operations.
  //

#if FUCHSIA_API_LEVEL_AT_LEAST(26)
  void DeprecatedClone(DeprecatedCloneRequestView request,
                       DeprecatedCloneCompleter::Sync& completer) final;
#else
  void Clone2(Clone2RequestView request, Clone2Completer::Sync& completer) final;
#endif
  void Clone(CloneRequestView request, CloneCompleter::Sync& completer) final;
  void Close(CloseCompleter::Sync& completer) final;
  void Query(QueryCompleter::Sync& completer) final;
  void GetConnectionInfo(GetConnectionInfoCompleter::Sync& completer) final;
  void Sync(SyncCompleter::Sync& completer) final;
  void GetAttr(GetAttrCompleter::Sync& completer) final;
  void SetAttr(SetAttrRequestView request, SetAttrCompleter::Sync& completer) final;
  void GetFlags(GetFlagsCompleter::Sync& completer) final;
  void SetFlags(SetFlagsRequestView request, SetFlagsCompleter::Sync& completer) final;
#if FUCHSIA_API_LEVEL_AT_LEAST(NEXT)
  void DeprecatedGetFlags(DeprecatedGetFlagsCompleter::Sync& completer) final;
  void DeprecatedSetFlags(DeprecatedSetFlagsRequestView request,
                          DeprecatedSetFlagsCompleter::Sync& completer) final;
#endif
  void QueryFilesystem(QueryFilesystemCompleter::Sync& completer) final;
  void GetAttributes(fuchsia_io::wire::NodeGetAttributesRequest* request,
                     GetAttributesCompleter::Sync& completer) final;
  void UpdateAttributes(fuchsia_io::wire::MutableNodeAttributes* request,
                        UpdateAttributesCompleter::Sync& completer) final;
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  void ListExtendedAttributes(ListExtendedAttributesRequestView request,
                              ListExtendedAttributesCompleter::Sync& completer) final {
    request->iterator.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void GetExtendedAttribute(GetExtendedAttributeRequestView request,
                            GetExtendedAttributeCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void SetExtendedAttribute(SetExtendedAttributeRequestView request,
                            SetExtendedAttributeCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void RemoveExtendedAttribute(RemoveExtendedAttributeRequestView request,
                               RemoveExtendedAttributeCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void LinkInto(fuchsia_io::wire::LinkableLinkIntoRequest* request,
                LinkIntoCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
#endif

  //
  // |fuchsia.io/File| operations.
  //

  void Describe(DescribeCompleter::Sync& completer) final;
  void Resize(ResizeRequestView request, ResizeCompleter::Sync& completer) final;
  void GetBackingMemory(GetBackingMemoryRequestView request,
                        GetBackingMemoryCompleter::Sync& completer) final;
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  void Allocate(AllocateRequestView request, AllocateCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void EnableVerity(EnableVerityRequestView request, EnableVerityCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
#endif
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_io::File>,
                             fidl::UnknownMethodCompleter::Sync&) override;
  //
  // |fuchsia.io/AdvisoryLocking| operations.
  //

  void AdvisoryLock(fidl::WireServer<fuchsia_io::File>::AdvisoryLockRequestView request,
                    AdvisoryLockCompleter::Sync& _completer) final;

  zx_status_t ResizeInternal(uint64_t length);
  zx_status_t GetBackingMemoryInternal(fuchsia_io::wire::VmoFlags flags, zx::vmo* out_vmo);

 private:
  std::optional<fidl::ServerBindingRef<fuchsia_io::File>> binding_;
  const zx_koid_t koid_;
};

}  // namespace fs::internal

#endif  // SRC_STORAGE_LIB_VFS_CPP_CONNECTION_FILE_CONNECTION_H_
