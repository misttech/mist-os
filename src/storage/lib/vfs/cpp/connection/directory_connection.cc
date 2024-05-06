// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/connection/directory_connection.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/zx/handle.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <zircon/assert.h>

#include <string_view>
#include <type_traits>
#include <utility>

#include <fbl/string_buffer.h>

#include "fidl/fuchsia.io/cpp/common_types.h"
#include "fidl/fuchsia.io/cpp/wire_types.h"
#include "src/storage/lib/vfs/cpp/connection/advisory_lock.h"
#include "src/storage/lib/vfs/cpp/debug.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs::internal {

namespace {

// Performs a path walk and opens a connection to another node.
void OpenAt(FuchsiaVfs* vfs, const fbl::RefPtr<Vnode>& parent,
            fidl::ServerEnd<fio::Node> server_end, std::string_view path,
            VnodeConnectionOptions options, fio::Rights parent_rights) {
  vfs->Open(parent, path, options, parent_rights)
      .visit([vfs, &server_end, options, path](auto&& result) {
        using ResultT = std::decay_t<decltype(result)>;
        using OpenResult = fs::Vfs::OpenResult;
        if constexpr (std::is_same_v<ResultT, OpenResult::Error>) {
          if (options.flags & fuchsia_io::OpenFlags::kDescribe) {
            // Ignore errors since there is nothing we can do if this fails.
            [[maybe_unused]] const fidl::Status unused_result =
                fidl::WireSendEvent(server_end)->OnOpen(result, fio::wire::NodeInfoDeprecated());
            server_end.reset();
          }
        } else if constexpr (std::is_same_v<ResultT, OpenResult::Remote>) {
          const fbl::RefPtr<const Vnode> vn = result.vnode;
          const std::string_view path = result.path;

          // Ignore errors since there is nothing we can do if this fails.
          [[maybe_unused]] const zx_status_t status =
              vn->OpenRemote(options.ToIoV1Flags(), {}, fidl::StringView::FromExternal(path),
                             std::move(server_end));
        } else if constexpr (std::is_same_v<ResultT, OpenResult::Ok>) {
          // TODO(https://fxbug.dev/42051879): Remove this when web_engine with SDK 13.20230626.3.1
          // or later is rolled. The important commit is in the private integration repo, but the
          // next Fuchsia commit is b615ff398580f3b47c050beb9e8f0fc28907ac67 which can be used with
          // the sdkrevisions tool.
          VnodeConnectionOptions options = result.options;
          if (options.ToIoV1Flags() == fio::OpenFlags::kRightReadable) {
            bool is_device =
                path.length() == 8 &&
                std::all_of(path.begin(), path.end(), [](char c) { return std::isxdigit(c); });
            if (path == "000" || is_device) {
              options.rights -= fio::Rights::kReadBytes;
            }
          }
          // |Vfs::Open| already performs option validation for us.
          [[maybe_unused]] zx_status_t status =
              vfs->Serve(result.vnode, server_end.TakeChannel(), options);
        }
      });
}

}  // namespace

DirectoryConnection::DirectoryConnection(fs::FuchsiaVfs* vfs, fbl::RefPtr<fs::Vnode> vnode,
                                         fuchsia_io::Rights rights, zx_koid_t koid)
    : Connection(vfs, std::move(vnode), rights), koid_(koid) {
  // Ensure the VFS does not create connections that have privileges which cannot be used.
  ZX_DEBUG_ASSERT(internal::DownscopeRights(rights, VnodeProtocol::kDirectory) == rights);
}

DirectoryConnection::~DirectoryConnection() {
  [[maybe_unused]] zx::result result = Unbind();
  vnode()->DeleteFileLockInTeardown(koid_);
}

void DirectoryConnection::BindImpl(zx::channel channel, OnUnbound on_unbound) {
  ZX_DEBUG_ASSERT(!binding_);
  binding_.emplace(fidl::BindServer(
      vfs()->dispatcher(), fidl::ServerEnd<fuchsia_io::Directory>{std::move(channel)}, this,
      [on_unbound = std::move(on_unbound)](DirectoryConnection* self, fidl::UnbindInfo,
                                           fidl::ServerEnd<fuchsia_io::Directory>) {
        on_unbound(self);
      }));
}

zx::result<> DirectoryConnection::Unbind() {
  if (std::optional binding = std::exchange(binding_, std::nullopt); binding) {
    binding->Unbind();
    return zx::make_result(vnode()->Close());
  }
  return zx::ok();
}

void DirectoryConnection::Clone(CloneRequestView request, CloneCompleter::Sync& completer) {
  Connection::NodeClone(request->flags, VnodeProtocol::kDirectory, std::move(request->object));
}

void DirectoryConnection::Close(CloseCompleter::Sync& completer) { completer.Reply(Unbind()); }

void DirectoryConnection::Query(QueryCompleter::Sync& completer) {
  std::string_view protocol = fio::kDirectoryProtocolName;
  // TODO(https://fxbug.dev/42052765): avoid the const cast.
  uint8_t* data = reinterpret_cast<uint8_t*>(const_cast<char*>(protocol.data()));
  completer.Reply(fidl::VectorView<uint8_t>::FromExternal(data, protocol.size()));
}

void DirectoryConnection::GetConnectionInfo(GetConnectionInfoCompleter::Sync& completer) {
  fidl::Arena arena;
  completer.Reply(fio::wire::ConnectionInfo::Builder(arena).rights(rights()).Build());
}

void DirectoryConnection::Sync(SyncCompleter::Sync& completer) {
  vnode()->Sync([completer = completer.ToAsync()](zx_status_t sync_status) mutable {
    if (sync_status != ZX_OK) {
      completer.ReplyError(sync_status);
    } else {
      completer.ReplySuccess();
    }
  });
}

void DirectoryConnection::GetAttr(GetAttrCompleter::Sync& completer) {
  zx::result result = Connection::NodeGetAttr();
  if (result.is_error()) {
    completer.Reply(result.status_value(), fio::wire::NodeAttributes());
  } else {
    completer.Reply(ZX_OK, result.value().ToIoV1NodeAttributes());
  }
}

void DirectoryConnection::SetAttr(SetAttrRequestView request, SetAttrCompleter::Sync& completer) {
  zx::result result = Connection::NodeSetAttr(request->flags, request->attributes);
  if (result.is_error()) {
    completer.Reply(result.status_value());
  } else {
    completer.Reply(ZX_OK);
  }
}

void DirectoryConnection::GetFlags(GetFlagsCompleter::Sync& completer) {
  completer.Reply(ZX_OK, RightsToOpenFlags(rights()));
}

void DirectoryConnection::SetFlags(SetFlagsRequestView request,
                                   SetFlagsCompleter::Sync& completer) {
  completer.Reply(ZX_ERR_NOT_SUPPORTED);
}

void DirectoryConnection::Open(OpenRequestView request, OpenCompleter::Sync& completer) {
  // TODO(https://fxbug.dev/324080764): This io1 operation should require the TRAVERSE right.
  zx_status_t status = [&]() {
    std::string_view path(request->path.data(), request->path.size());
    fio::OpenFlags flags = request->flags;
    if (path.empty() || ((path == "." || path == "/") && (flags & fio::OpenFlags::kNotDirectory))) {
      return ZX_ERR_INVALID_ARGS;
    }
    if (path.back() == '/') {
      flags |= fio::OpenFlags::kDirectory;
    }
    zx::result open_options = VnodeConnectionOptions::FromOpen1Flags(flags);
    if (open_options.is_error()) {
      FS_PRETTY_TRACE_DEBUG("[DirectoryOpen] invalid flags: ", request->flags,
                            ", path: ", request->path);
      return open_options.error_value();
    }
    FS_PRETTY_TRACE_DEBUG("[DirectoryOpen] our rights ", rights(),
                          ", incoming options: ", *open_options, ", path: ", path);
    // The POSIX compatibility flags allow the child directory connection to inherit the writable
    // and executable rights.  If there exists a directory without the corresponding right along
    // the Open() chain, we remove that POSIX flag preventing it from being inherited down the line
    // (this applies both for local and remote mount points, as the latter may be served using
    // a connection with vastly greater rights).
    if (!(rights() & fio::Rights::kWriteBytes)) {
      open_options->flags &= ~fio::OpenFlags::kPosixWritable;
    }
    if (!(rights() & fio::Rights::kExecute)) {
      open_options->flags &= ~fio::OpenFlags::kPosixExecutable;
    }
    // Return ACCESS_DENIED if the client asked for a right the parent connection doesn't have.
    if (open_options->rights - rights()) {
      return ZX_ERR_ACCESS_DENIED;
    }
    OpenAt(vfs(), vnode(), std::move(request->object), path, *open_options, rights());
    return ZX_OK;
  }();

  if (status != ZX_OK) {
    FS_PRETTY_TRACE_DEBUG("[DirectoryOpen] error: ", status);
    if (request->flags & fio::wire::OpenFlags::kDescribe) {
      // Ignore errors since there is nothing we can do if this fails.
      [[maybe_unused]] auto result = fidl::WireSendEvent(request->object)->OnOpen(status, {});
    }
  }
}

void DirectoryConnection::Unlink(UnlinkRequestView request, UnlinkCompleter::Sync& completer) {
  FS_PRETTY_TRACE_DEBUG("[DirectoryUnlink] our rights: ", rights(), ", name: ", request->name);
  // TODO(https://fxbug.dev/324080764): This operation should require ENUMERATE and MODIFY_DIRECTORY
  // rights, instead of WRITE_BYTES.
  if (!(rights() & fuchsia_io::Rights::kWriteBytes)) {
    completer.ReplyError(ZX_ERR_BAD_HANDLE);
    return;
  }
  std::string_view name_str(request->name.data(), request->name.size());
  if (!IsValidName(name_str)) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  zx_status_t status =
      vfs()->Unlink(vnode(), name_str,
                    request->options.has_flags() &&
                        static_cast<bool>((request->options.flags() &
                                           fuchsia_io::wire::UnlinkFlags::kMustBeDirectory)));
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void DirectoryConnection::ReadDirents(ReadDirentsRequestView request,
                                      ReadDirentsCompleter::Sync& completer) {
  FS_PRETTY_TRACE_DEBUG("[DirectoryReadDirents] our rights: ", rights());
  // TODO(https://fxbug.dev/324080764): This io1 operation should require the ENUMERATE right.
  if (request->max_bytes > fio::wire::kMaxBuf) {
    completer.Reply(ZX_ERR_BAD_HANDLE, fidl::VectorView<uint8_t>());
    return;
  }
  uint8_t data[request->max_bytes];
  size_t actual = 0;
  zx_status_t status =
      vfs()->Readdir(vnode().get(), &dircookie_, data, request->max_bytes, &actual);
  completer.Reply(status, fidl::VectorView<uint8_t>::FromExternal(data, actual));
}

void DirectoryConnection::Rewind(RewindCompleter::Sync& completer) {
  FS_PRETTY_TRACE_DEBUG("[DirectoryRewind] our rights: ", rights());
  // TODO(https://fxbug.dev/324080764): This io1 operation should require the ENUMERATE right.
  dircookie_ = VdirCookie();
  completer.Reply(ZX_OK);
}

void DirectoryConnection::GetToken(GetTokenCompleter::Sync& completer) {
  FS_PRETTY_TRACE_DEBUG("[DirectoryGetToken] our rights: ", rights());
  // TODO(https://fxbug.dev/324080764): This io1 operation should need ENUMERATE or another right.
  if (!(rights() & fuchsia_io::Rights::kWriteBytes)) {
    completer.Reply(ZX_ERR_BAD_HANDLE, zx::handle());
    return;
  }
  zx::event returned_token;
  zx_status_t status = vfs()->VnodeToToken(vnode(), &token(), &returned_token);
  completer.Reply(status, std::move(returned_token));
}

void DirectoryConnection::Rename(RenameRequestView request, RenameCompleter::Sync& completer) {
  FS_PRETTY_TRACE_DEBUG("[DirectoryRename] our rights: ", rights(), ", src: ", request->src,
                        ", dst: ", request->dst);
  if (request->src.empty() || request->dst.empty()) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  // TODO(https://fxbug.dev/324080764): This operation should require the MODIFY_DIRECTORY right
  // instead of the WRITE_BYTES right.
  if (!(rights() & fuchsia_io::Rights::kWriteBytes)) {
    completer.ReplyError(ZX_ERR_BAD_HANDLE);
    return;
  }
  zx_status_t status = vfs()->Rename(std::move(request->dst_parent_token), vnode(),
                                     std::string_view(request->src.data(), request->src.size()),
                                     std::string_view(request->dst.data(), request->dst.size()));
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void DirectoryConnection::Link(LinkRequestView request, LinkCompleter::Sync& completer) {
  FS_PRETTY_TRACE_DEBUG("[DirectoryLink] our rights: ", rights(), ", src: ", request->src,
                        ", dst: ", request->dst);
  // |fuchsia.io/Directory.Rename| only specified the token to be a generic handle; casting it here.
  zx::event token(request->dst_parent_token.release());
  if (request->src.empty() || request->dst.empty()) {
    completer.Reply(ZX_ERR_INVALID_ARGS);
    return;
  }
  // TODO(https://fxbug.dev/324080764): This operation should require the MODIFY_DIRECTORY right
  // instead of the WRITE_BYTES right.
  if (!(rights() & fuchsia_io::Rights::kWriteBytes)) {
    completer.Reply(ZX_ERR_BAD_HANDLE);
    return;
  }
  zx_status_t status = vfs()->Link(std::move(token), vnode(),
                                   std::string_view(request->src.data(), request->src.size()),
                                   std::string_view(request->dst.data(), request->dst.size()));
  completer.Reply(status);
}

void DirectoryConnection::Watch(WatchRequestView request, WatchCompleter::Sync& completer) {
  FS_PRETTY_TRACE_DEBUG("[DirectoryWatch] our rights: ", rights());
  // TODO(https://fxbug.dev/324080764): This io1 operation should require the ENUMERATE right.
  zx_status_t status =
      vnode()->WatchDir(vfs(), request->mask, request->options, std::move(request->watcher));
  completer.Reply(status);
}

void DirectoryConnection::QueryFilesystem(QueryFilesystemCompleter::Sync& completer) {
  FS_PRETTY_TRACE_DEBUG("[DirectoryQueryFilesystem] our rights: ", rights());

  zx::result result = Connection::NodeQueryFilesystem();
  completer.Reply(result.status_value(),
                  result.is_ok() ? fidl::ObjectView<fuchsia_io::wire::FilesystemInfo>::FromExternal(
                                       &result.value())
                                 : nullptr);
}

void DirectoryConnection::AdvisoryLock(AdvisoryLockRequestView request,
                                       AdvisoryLockCompleter::Sync& completer) {
  // advisory_lock replies to the completer
  auto async_completer = completer.ToAsync();
  fit::callback<void(zx_status_t)> callback = file_lock::lock_completer_t(
      [lock_completer = std::move(async_completer)](zx_status_t status) mutable {
        lock_completer.ReplyError(status);
      });

  advisory_lock(koid_, vnode(), false, request->request, std::move(callback));
}

zx::result<> DirectoryConnection::WithRepresentation(
    fit::callback<void(fuchsia_io::wire::Representation)> handler,
    std::optional<fuchsia_io::NodeAttributesQuery> query) const {
  // TODO(https://fxbug.dev/324112857): Handle |query|.
  if (query) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  fidl::WireTableFrame<fuchsia_io::wire::DirectoryInfo> frame;
  auto builder = fuchsia_io::wire::DirectoryInfo::ExternalBuilder(
      fidl::ObjectView<fidl::WireTableFrame<fuchsia_io::wire::DirectoryInfo>>::FromExternal(
          &frame));
  auto info = builder.Build();
  auto representation = fuchsia_io::wire::Representation::WithDirectory(
      fidl::ObjectView<fuchsia_io::wire::DirectoryInfo>::FromExternal(&info));
  handler(std::move(representation));
  return zx::ok();
}

zx::result<> DirectoryConnection::WithNodeInfoDeprecated(
    fit::callback<void(fuchsia_io::wire::NodeInfoDeprecated)> handler) const {
  handler(fuchsia_io::wire::NodeInfoDeprecated::WithDirectory({}));
  return zx::ok();
}

}  // namespace fs::internal
