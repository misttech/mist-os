// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/node_connection.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/zx/handle.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <zircon/assert.h>

#include <memory>
#include <utility>

#include <fbl/string_buffer.h>

#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs {

namespace internal {

NodeConnection::NodeConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode,
                               VnodeProtocol protocol, VnodeConnectionOptions options)
    : Connection(vfs, std::move(vnode), protocol, options) {
  ZX_DEBUG_ASSERT(protocol == VnodeProtocol::kNode);
  ZX_DEBUG_ASSERT(options.flags & fio::OpenFlags::kNodeReference);
  // Only the GET_ATTRIBUTES right is supported for node connections.
  ZX_DEBUG_ASSERT(!(options.rights & ~fio::Rights::kGetAttributes));
}

std::unique_ptr<Binding> NodeConnection::Bind(async_dispatcher_t* dispatcher, zx::channel channel,
                                              OnUnbound on_unbound) {
  return std::make_unique<TypedBinding<fio::Node>>(fidl::BindServer(
      dispatcher, fidl::ServerEnd<fio::Node>{std::move(channel)}, this,
      [on_unbound = std::move(on_unbound)](NodeConnection* self, fidl::UnbindInfo,
                                           fidl::ServerEnd<fio::Node>) { on_unbound(self); }));
}

void NodeConnection::Clone(CloneRequestView request, CloneCompleter::Sync& completer) {
  // The NODE_REFERENCE flag should be preserved when cloning a node connection.
  Connection::NodeClone(request->flags | fio::OpenFlags::kNodeReference,
                        std::move(request->object));
}

void NodeConnection::Close(CloseCompleter::Sync& completer) {
  zx::result<> result = Connection::NodeClose();
  completer.Reply(result);
}

void NodeConnection::Query(QueryCompleter::Sync& completer) {
  completer.Reply(Connection::NodeQuery());
}

void NodeConnection::GetConnectionInfo(GetConnectionInfoCompleter::Sync& completer) {
  fidl::Arena arena;
  completer.Reply(fio::wire::ConnectionInfo::Builder(arena).rights(rights()).Build());
}

void NodeConnection::Sync(SyncCompleter::Sync& completer) {
  Connection::NodeSync([completer = completer.ToAsync()](zx_status_t status) mutable {
    completer.Reply(zx::make_result(status));
  });
}

void NodeConnection::GetAttr(GetAttrCompleter::Sync& completer) {
  zx::result result = Connection::NodeGetAttr();
  completer.Reply(result.status_value(), result.is_ok() ? result.value().ToIoV1NodeAttributes()
                                                        : fio::wire::NodeAttributes());
}

void NodeConnection::SetAttr(SetAttrRequestView request, SetAttrCompleter::Sync& completer) {
  zx::result<> result = Connection::NodeSetAttr(request->flags, request->attributes);
  completer.Reply(result.status_value());
}

void NodeConnection::GetFlags(GetFlagsCompleter::Sync& completer) {
  completer.Reply(ZX_OK, Connection::NodeGetFlags());
}

void NodeConnection::SetFlags(SetFlagsRequestView request, SetFlagsCompleter::Sync& completer) {
  completer.Reply(ZX_ERR_BAD_HANDLE);
}

void NodeConnection::QueryFilesystem(QueryFilesystemCompleter::Sync& completer) {
  zx::result result = Connection::NodeQueryFilesystem();
  completer.Reply(result.status_value(),
                  result.is_ok()
                      ? fidl::ObjectView<fio::wire::FilesystemInfo>::FromExternal(&result.value())
                      : nullptr);
}

}  // namespace internal

}  // namespace fs
