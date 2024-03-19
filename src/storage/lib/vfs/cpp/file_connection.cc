// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/file_connection.h"

#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/zx/handle.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <zircon/assert.h>

#include <memory>
#include <utility>

#include <fbl/string_buffer.h>

#include "src/storage/lib/vfs/cpp/advisory_lock.h"
#include "src/storage/lib/vfs/cpp/debug.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs {

namespace internal {

FileConnection::FileConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode,
                               fuchsia_io::Rights rights, bool append, zx_koid_t koid)
    : Connection(vfs, std::move(vnode), fs::VnodeProtocol::kFile, rights),
      koid_(koid),
      append_(append) {}

FileConnection::~FileConnection() { vnode()->DeleteFileLockInTeardown(koid_); }

std::unique_ptr<Binding> FileConnection::Bind(async_dispatcher_t* dispatcher, zx::channel channel,
                                              OnUnbound on_unbound) {
  return std::make_unique<TypedBinding<fio::File>>(fidl::BindServer(
      dispatcher, fidl::ServerEnd<fio::File>{std::move(channel)}, this,
      [on_unbound = std::move(on_unbound)](FileConnection* self, fidl::UnbindInfo,
                                           fidl::ServerEnd<fio::File>) { on_unbound(self); }));
}

void FileConnection::Clone(CloneRequestView request, CloneCompleter::Sync& completer) {
  fio::OpenFlags inherited_flags = {};
  // The APPEND flag should be preserved when cloning a file connection.
  if (append()) {
    inherited_flags |= fio::OpenFlags::kAppend;
  }
  Connection::NodeClone(request->flags | inherited_flags, std::move(request->object));
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

zx::result<fs::VnodeRepresentation> FileConnection::NodeGetRepresentation() const {
  fuchsia_io::FileInfo info;
  zx::result<zx::event> observer = vnode()->GetObserver();
  if (observer.is_ok()) {
    info.observer() = std::move(*observer);
  } else if (observer.error_value() != ZX_ERR_NOT_SUPPORTED) {
    return observer.take_error();
  }
  return zx::ok(std::move(info));
}

void FileConnection::Describe(DescribeCompleter::Sync& completer) {
  zx::result representation = NodeGetRepresentation();
  if (representation.is_error()) {
    completer.Close(representation.status_value());
    return;
  }
  fuchsia_io::FileInfo* info = std::get_if<fuchsia_io::FileInfo>(&*representation);
  ZX_DEBUG_ASSERT(info);
  fidl::Arena arena;
  completer.Reply(fidl::ToWire(arena, std::move(*info)));
}

void FileConnection::GetConnectionInfo(GetConnectionInfoCompleter::Sync& completer) {
  // Filter out directory-only operations for consistency with Rust VFS.
  // TODO(https://fxbug.dev/324112857): When adding Open2 support, use the node's abilities to
  // filter out unsupported rights. Although the protocol currently allows rights to exceed the
  // abilities of a node, this can be confusing. Instead, we can make it such that rights can
  // only be downscoped, and can only be requested if the node type supports or allows it.
  fio::Rights file_rights = rights() & ~(fio::Rights::kConnect | fio::Rights::kModifyDirectory |
                                         fio::Rights::kEnumerate | fio::Rights::kTraverse);
  fidl::Arena arena;
  completer.Reply(fio::wire::ConnectionInfo::Builder(arena).rights(file_rights).Build());
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

void FileConnection::GetFlags(GetFlagsCompleter::Sync& completer) {
  completer.Reply(ZX_OK, NodeGetFlags());
}

void FileConnection::SetFlags(SetFlagsRequestView request, SetFlagsCompleter::Sync& completer) {
  append() = static_cast<bool>(request->flags & fio::OpenFlags::kAppend);
  completer.Reply(ZX_OK);
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
  if (!(rights() & fuchsia_io::Rights::kWriteBytes)) {
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
  if ((flags & fio::VmoFlags::kPrivateClone) && (flags & fio::VmoFlags::kSharedBuffer)) {
    return ZX_ERR_INVALID_ARGS;
  }
  if ((flags & fio::VmoFlags::kRead) && !(rights() & fio::Rights::kReadBytes)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if ((flags & fio::VmoFlags::kWrite) && !(rights() & fio::Rights::kWriteBytes)) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if ((flags & fio::VmoFlags::kExecute) && !(rights() & fio::Rights::kExecute)) {
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

fio::OpenFlags FileConnection::NodeGetFlags() const {
  fio::OpenFlags flags = Connection::NodeGetFlags();
  if (append()) {
    flags |= fio::OpenFlags::kAppend;
  }
  return flags;
}

}  // namespace internal

}  // namespace fs
