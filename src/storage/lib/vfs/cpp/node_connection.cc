// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/node_connection.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/zx/handle.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <zircon/assert.h>

#include <utility>

#include <fbl/string_buffer.h>

#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs {

namespace internal {

NodeConnection::NodeConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode,
                               fuchsia_io::Rights rights)
    : Connection(vfs, std::move(vnode), rights) {
  // Only Rights.GET_ATTRIBUTES is allowed for node connections.
  ZX_DEBUG_ASSERT(!(rights - fio::Rights::kGetAttributes));
}

NodeConnection::~NodeConnection() { [[maybe_unused]] zx::result result = Unbind(); }

void NodeConnection::BindImpl(zx::channel channel, OnUnbound on_unbound) {
  ZX_DEBUG_ASSERT(!binding_);
  binding_.emplace(fidl::BindServer(vfs()->dispatcher(),
                                    fidl::ServerEnd<fuchsia_io::Node>{std::move(channel)}, this,
                                    [on_unbound = std::move(on_unbound)](
                                        NodeConnection* self, fidl::UnbindInfo,
                                        fidl::ServerEnd<fuchsia_io::Node>) { on_unbound(self); }));
}

zx::result<> NodeConnection::Unbind() {
  if (std::optional binding = std::exchange(binding_, std::nullopt); binding) {
    binding->Unbind();
  }
  return zx::ok();
}
void NodeConnection::Clone(CloneRequestView request, CloneCompleter::Sync& completer) {
  // The NODE_REFERENCE flag should be preserved when cloning a node connection.
  Connection::NodeClone(request->flags | fio::OpenFlags::kNodeReference,
                        std::move(request->object));
}

void NodeConnection::Close(CloseCompleter::Sync& completer) { completer.Reply(Unbind()); }

void NodeConnection::Query(QueryCompleter::Sync& completer) {
  std::string_view protocol = fio::kNodeProtocolName;
  // TODO(https://fxbug.dev/42052765): avoid the const cast.
  uint8_t* data = reinterpret_cast<uint8_t*>(const_cast<char*>(protocol.data()));
  completer.Reply(fidl::VectorView<uint8_t>::FromExternal(data, protocol.size()));
}

void NodeConnection::GetConnectionInfo(GetConnectionInfoCompleter::Sync& completer) {
  fidl::Arena arena;
  completer.Reply(fio::wire::ConnectionInfo::Builder(arena).rights(rights()).Build());
}

void NodeConnection::Sync(SyncCompleter::Sync& completer) {
  completer.Reply(zx::make_result(ZX_ERR_BAD_HANDLE));
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
  completer.Reply(ZX_OK, fio::OpenFlags::kNodeReference);
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
