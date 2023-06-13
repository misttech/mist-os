// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_STORAGE_VFS_CPP_DIRECTORY_CONNECTION_H_
#define SRC_LIB_STORAGE_VFS_CPP_DIRECTORY_CONNECTION_H_

#ifndef __Fuchsia__
#error "Fuchsia-only header"
#endif

#include "src/lib/storage/vfs/cpp/connection.h"
#include "src/lib/storage/vfs/cpp/vfs.h"
#include "src/lib/storage/vfs/cpp/vfs_types.h"
#include "src/lib/storage/vfs/cpp/vnode.h"

namespace fs {

namespace internal {

class DirectoryConnection final : public Connection,
                                  public fidl::WireServer<fuchsia_io::Directory> {
 public:
  // Refer to documentation for |Connection::Connection|.
  DirectoryConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode, VnodeProtocol protocol,
                      VnodeConnectionOptions options, zx_koid_t koid);

  ~DirectoryConnection() final;

 private:
  std::unique_ptr<Binding> Bind(async_dispatcher*, zx::channel, OnUnbound) override;

  //
  // |fuchsia.io/Node| operations.
  //

  void Clone(CloneRequestView request, CloneCompleter::Sync& completer) final;
  void Close(CloseCompleter::Sync& completer) final;
  void Query(QueryCompleter::Sync& completer) final;
  void GetConnectionInfo(GetConnectionInfoCompleter::Sync& completer) final;
  void Sync(SyncCompleter::Sync& completer) final;
  void GetAttr(GetAttrCompleter::Sync& completer) final;
  void SetAttr(SetAttrRequestView request, SetAttrCompleter::Sync& completer) final;
  void GetFlags(GetFlagsCompleter::Sync& completer) final;
  void SetFlags(SetFlagsRequestView request, SetFlagsCompleter::Sync& completer) final;
  void GetAttributes(fuchsia_io::wire::Node2GetAttributesRequest* request,
                     GetAttributesCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void UpdateAttributes(fuchsia_io::wire::MutableNodeAttributes* request,
                        UpdateAttributesCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
  void Reopen(fuchsia_io::wire::Node2ReopenRequest* request,
              ReopenCompleter::Sync& completer) final {
    request->object_request.Close(ZX_ERR_NOT_SUPPORTED);
  }
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
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
#endif

  //
  // |fuchsia.io/Directory| operations.
  //

  void Open(OpenRequestView request, OpenCompleter::Sync& completer) final;
  void Unlink(UnlinkRequestView request, UnlinkCompleter::Sync& completer) final;
  void ReadDirents(ReadDirentsRequestView request, ReadDirentsCompleter::Sync& completer) final;
  void Rewind(RewindCompleter::Sync& completer) final;
  void GetToken(GetTokenCompleter::Sync& completer) final;
  void Rename(RenameRequestView request, RenameCompleter::Sync& completer) final;
  void Link(LinkRequestView request, LinkCompleter::Sync& completer) final;
  void Watch(WatchRequestView request, WatchCompleter::Sync& completer) final;
  void QueryFilesystem(QueryFilesystemCompleter::Sync& completer) final;
  void Open2(fuchsia_io::wire::Directory2Open2Request* request,
             Open2Completer::Sync& completer) final {
    fidl::ServerEnd<fuchsia_io::Node>(std::move(request->object_request))
        .Close(ZX_ERR_NOT_SUPPORTED);
  }
  void Enumerate(fuchsia_io::wire::Directory2EnumerateRequest* request,
                 EnumerateCompleter::Sync& completer) final {
    request->iterator.Close(ZX_ERR_NOT_SUPPORTED);
  }
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  void CreateSymlink(fuchsia_io::wire::Directory2CreateSymlinkRequest* request,
                     CreateSymlinkCompleter::Sync& completer) final {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }
#endif

  //
  // |fuchsia.io/AdvisoryLocking| operations.
  //

  void AdvisoryLock(AdvisoryLockRequestView request, AdvisoryLockCompleter::Sync& _completer) final;

  // Directory cookie for readdir operations.
  fs::VdirCookie dircookie_;

  const zx_koid_t koid_;
};

}  // namespace internal

}  // namespace fs

#endif  // SRC_LIB_STORAGE_VFS_CPP_DIRECTORY_CONNECTION_H_
