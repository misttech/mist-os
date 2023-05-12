// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/storage/vfs/cpp/file_connection.h"

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

#include "src/lib/storage/vfs/cpp/advisory_lock.h"
#include "src/lib/storage/vfs/cpp/debug.h"
#include "src/lib/storage/vfs/cpp/vfs_types.h"
#include "src/lib/storage/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs {

namespace internal {

FileConnection::FileConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode,
                               VnodeProtocol protocol, VnodeConnectionOptions options,
                               zx_koid_t koid)
    : Connection(vfs, std::move(vnode), protocol, options), koid_(koid) {}

FileConnection::~FileConnection() { vnode()->DeleteFileLockInTeardown(koid_); }

std::unique_ptr<Binding> FileConnection::Bind(async_dispatcher_t* dispatcher, zx::channel channel,
                                              OnUnbound on_unbound) {
  return std::make_unique<TypedBinding<fio::File>>(fidl::BindServer(
      dispatcher, fidl::ServerEnd<fio::File>{std::move(channel)}, this,
      [on_unbound = std::move(on_unbound)](FileConnection* self, fidl::UnbindInfo,
                                           fidl::ServerEnd<fio::File>) { on_unbound(self); }));
}

void FileConnection::Clone(CloneRequestView request, CloneCompleter::Sync& completer) {
  Connection::NodeClone(request->flags, std::move(request->object));
}

void FileConnection::Close(CloseCompleter::Sync& completer) {
  zx::result result = Connection::NodeClose();
  if (result.is_error()) {
    completer.ReplyError(result.status_value());
  } else {
    completer.ReplySuccess();
  }
}

void FileConnection::Query(QueryCompleter::Sync& completer) {
  completer.Reply(Connection::NodeQuery());
}

void FileConnection::Describe(DescribeCompleter::Sync& completer) {
  zx::result result = Connection::NodeDescribe();
  if (result.is_error()) {
    return completer.Close(result.status_value());
  }
  ConnectionInfoConverter converter(std::move(result).value());
  switch (converter.representation.Which()) {
    default:
    case fio::wire::Representation::Tag::kConnector:
    case fio::wire::Representation::Tag::kDirectory:
      return completer.Close(ZX_ERR_BAD_STATE);
    case fio::wire::Representation::Tag::kFile:
      fio::wire::FileInfo file = converter.representation.file();
      return completer.Reply(file);
  }
}

void FileConnection::GetConnectionInfo(GetConnectionInfoCompleter::Sync& completer) {
  fio::Operations rights = fio::Operations::kGetAttributes;
  if (!options().flags.node_reference) {
    rights |= options().rights.read ? fio::Operations::kReadBytes : fio::Operations();
    rights |= options().rights.write
                  ? fio::Operations::kWriteBytes | fio::Operations::kUpdateAttributes
                  : fio::Operations();
    rights |= options().rights.execute ? fio::Operations::kExecute : fio::Operations();
  }
  fidl::Arena arena;
  completer.Reply(fio::wire::ConnectionInfo::Builder(arena).rights(rights).Build());
}

void FileConnection::Sync(SyncCompleter::Sync& completer) {
  Connection::NodeSync([completer = completer.ToAsync()](zx_status_t sync_status) mutable {
    if (sync_status != ZX_OK) {
      completer.ReplyError(sync_status);
    } else {
      completer.ReplySuccess();
    }
  });
}

void FileConnection::GetAttr(GetAttrCompleter::Sync& completer) {
  zx::result result = Connection::NodeGetAttr();
  if (result.is_error()) {
    completer.Reply(result.status_value(), fio::wire::NodeAttributes());
  } else {
    completer.Reply(ZX_OK, result.value().ToIoV1NodeAttributes());
  }
}

void FileConnection::SetAttr(SetAttrRequestView request, SetAttrCompleter::Sync& completer) {
  zx::result result = Connection::NodeSetAttr(request->flags, request->attributes);
  if (result.is_error()) {
    completer.Reply(result.status_value());
  } else {
    completer.Reply(ZX_OK);
  }
}

void FileConnection::QueryFilesystem(QueryFilesystemCompleter::Sync& completer) {
  zx::result result = Connection::NodeQueryFilesystem();
  completer.Reply(result.status_value(),
                  result.is_ok()
                      ? fidl::ObjectView<fio::wire::FilesystemInfo>::FromExternal(&result.value())
                      : nullptr);
}

zx_status_t FileConnection::ResizeInternal(uint64_t length) {
  FS_PRETTY_TRACE_DEBUG("[FileTruncate] options: ", options());

  if (options().flags.node_reference) {
    return ZX_ERR_BAD_HANDLE;
  }
  if (!options().rights.write) {
    return ZX_ERR_BAD_HANDLE;
  }

  return vnode()->Truncate(length);
}

void FileConnection::Resize(ResizeRequestView request, ResizeCompleter::Sync& completer) {
  zx_status_t result = ResizeInternal(request->length);
  if (result != ZX_OK) {
    completer.ReplyError(result);
  } else {
    completer.ReplySuccess();
  }
}

zx_status_t FileConnection::GetBackingMemoryInternal(fio::wire::VmoFlags flags, zx::vmo* out_vmo) {
  if (options().flags.node_reference) {
    return ZX_ERR_BAD_HANDLE;
  }
  if ((flags & fio::wire::VmoFlags::kPrivateClone) &&
      (flags & fio::wire::VmoFlags::kSharedBuffer)) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (!options().rights.write && (flags & fio::wire::VmoFlags::kWrite)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if (!options().rights.execute && (flags & fio::wire::VmoFlags::kExecute)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if (!options().rights.read && (flags & fio::wire::VmoFlags::kRead)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  return vnode()->GetVmo(flags, out_vmo);
}

void FileConnection::GetBackingMemory(GetBackingMemoryRequestView request,
                                      GetBackingMemoryCompleter::Sync& completer) {
  zx::vmo vmo;
  zx_status_t status = GetBackingMemoryInternal(request->flags, &vmo);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess(std::move(vmo));
  }
}

void FileConnection::AdvisoryLock(fidl::WireServer<fio::File>::AdvisoryLockRequestView request,
                                  AdvisoryLockCompleter::Sync& completer) {
  // advisory_lock replies to the completer
  auto async_completer = completer.ToAsync();
  fit::callback<void(zx_status_t)> callback = file_lock::lock_completer_t(
      [lock_completer = std::move(async_completer)](zx_status_t status) mutable {
        lock_completer.ReplyError(status);
      });

  advisory_lock(koid_, vnode(), true, request->request, std::move(callback));
}

}  // namespace internal

}  // namespace fs
