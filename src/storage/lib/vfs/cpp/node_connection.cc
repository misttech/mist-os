// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/node_connection.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fdio/io.h>
#include <lib/fdio/vfs.h>
#include <lib/zx/handle.h>
#include <limits.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <zircon/assert.h>

#include <memory>
#include <type_traits>
#include <utility>

#include <fbl/string_buffer.h>

#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs {

namespace internal {

NodeConnection::NodeConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode,
                               VnodeProtocol protocol, VnodeConnectionOptions options)
    : Connection(vfs, std::move(vnode), protocol, options) {}

std::unique_ptr<Binding> NodeConnection::Bind(async_dispatcher_t* dispatcher, zx::channel channel,
                                              OnUnbound on_unbound) {
  return std::make_unique<TypedBinding<fio::Node>>(fidl::BindServer(
      dispatcher, fidl::ServerEnd<fio::Node>{std::move(channel)}, this,
      [on_unbound = std::move(on_unbound)](NodeConnection* self, fidl::UnbindInfo,
                                           fidl::ServerEnd<fio::Node>) { on_unbound(self); }));
}

void NodeConnection::Clone(CloneRequestView request, CloneCompleter::Sync& completer) {
  Connection::NodeClone(request->flags, std::move(request->object));
}

void NodeConnection::Close(CloseCompleter::Sync& completer) {
  zx::result<> result = Connection::NodeClose();
  completer.Reply(result);
}

void NodeConnection::Query(QueryCompleter::Sync& completer) {
  if (options().flags.node_reference) {
    const std::string_view kProtocol = fio::wire::kNodeProtocolName;
    // TODO(https://fxbug.dev/101890): avoid the const cast.
    uint8_t* data = reinterpret_cast<uint8_t*>(const_cast<char*>(kProtocol.data()));
    completer.Reply(fidl::VectorView<uint8_t>::FromExternal(data, kProtocol.size()));
  } else {
    completer.Reply(Connection::NodeQuery());
  }
}

void NodeConnection::GetConnectionInfo(GetConnectionInfoCompleter::Sync& completer) {
  using fuchsia_io::Operations;
  using fuchsia_io::wire::ConnectionInfo;

  fidl::Arena arena;
  completer.Reply(ConnectionInfo::Builder(arena).rights(Operations::kGetAttributes).Build());
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
  zx::result result = Connection::NodeGetFlags();
  completer.Reply(result.status_value(), result.is_ok() ? result.value() : fio::wire::OpenFlags{});
}

void NodeConnection::SetFlags(SetFlagsRequestView request, SetFlagsCompleter::Sync& completer) {
  zx::result<> result = Connection::NodeSetFlags(request->flags);
  completer.Reply(result.status_value());
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
