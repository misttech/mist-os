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
                               fuchsia_io::Rights rights)
    : Connection(vfs, std::move(vnode), fs::VnodeProtocol::kNode, rights) {
  // Only Rights.GET_ATTRIBUTES is allowed for node connections.
  ZX_DEBUG_ASSERT(!(rights - fio::Rights::kGetAttributes));
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

zx::result<> NodeConnection::WithRepresentation(
    fit::callback<void(fuchsia_io::wire::Representation)> handler,
    std::optional<fuchsia_io::NodeAttributesQuery> query) const {
  // TODO(https://fxbug.dev/324112857): Handle |query|.
  if (query) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  fidl::WireTableFrame<fuchsia_io::wire::ConnectorInfo> info_frame;
  auto info_builder = fuchsia_io::wire::ConnectorInfo::ExternalBuilder(
      fidl::ObjectView<fidl::WireTableFrame<fuchsia_io::wire::ConnectorInfo>>::FromExternal(
          &info_frame));
  auto info = info_builder.Build();
  auto representation = fuchsia_io::wire::Representation::WithConnector(
      fidl::ObjectView<fuchsia_io::wire::ConnectorInfo>::FromExternal(&info));
  handler(std::move(representation));
  return zx::ok();
}

zx::result<> NodeConnection::WithNodeInfoDeprecated(
    fit::callback<void(fuchsia_io::wire::NodeInfoDeprecated)> handler) const {
  // In io1, node reference connections are mapped to the service variant of NodeInfoDeprecated.
  handler(fuchsia_io::wire::NodeInfoDeprecated::WithService({}));
  return zx::ok();
}

}  // namespace internal

}  // namespace fs
