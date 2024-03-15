// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/fuchsia_vfs.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/zx/event.h>
#include <lib/zx/process.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/assert.h>

#include <memory>
#include <string_view>
#include <utility>

#include <fbl/auto_lock.h>
#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/connection.h"
#include "src/storage/lib/vfs/cpp/directory_connection.h"
#include "src/storage/lib/vfs/cpp/node_connection.h"
#include "src/storage/lib/vfs/cpp/remote_file_connection.h"
#include "src/storage/lib/vfs/cpp/stream_file_connection.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

namespace fs {
namespace {

zx::result<zx_koid_t> GetObjectKoid(const zx::object_base& object) {
  zx_info_handle_basic_t info = {};
  if (zx_status_t status =
          object.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(info.koid);
}

uint32_t ToStreamOptions(const VnodeConnectionOptions& options) {
  uint32_t stream_options = 0u;
  if (options.rights & fuchsia_io::Rights::kReadBytes) {
    stream_options |= ZX_STREAM_MODE_READ;
  }
  if (options.rights & fuchsia_io::Rights::kWriteBytes) {
    stream_options |= ZX_STREAM_MODE_WRITE;
  }
  if (options.flags & fuchsia_io::OpenFlags::kAppend) {
    stream_options |= ZX_STREAM_MODE_APPEND;
  }
  return stream_options;
}

zx_status_t ConnectService(const fbl::RefPtr<fs::Vnode>& vnode, Vnode::ValidatedOptions options,
                           fidl::ServerEnd<fio::Node> server_end) {
  // For service connections, we don't know the final protocol that will be spoken over |server_end|
  // as it is dependent on the service Vnode type/connector. However, if |fio::OpenFlags::kDescribe|
  // is set, the channel first speaks |fuchsia.io/Node| to send the OnOpen event, then switches to
  // the service protocol.
  //
  // TODO(https://fxbug.dev/324111653): In io2/Open2, service connections do not support the
  // OnRepresentation event, and use of epitaphs when closing channels is explicitly defined.
  // We will likely need to remove support for protocol switching in io1 to support the migration.
  if (options->ToIoV1Flags() & ~(fio::OpenFlags::kDescribe | fio::OpenFlags::kNotDirectory)) {
    constexpr zx_status_t kStatus = ZX_ERR_INVALID_ARGS;
    if (options->flags & fio::OpenFlags::kDescribe) {
      [[maybe_unused]] fidl::Status status = fidl::WireSendEvent(server_end)->OnOpen(kStatus, {});
    } else {
      [[maybe_unused]] zx_status_t status = server_end.Close(kStatus);
    }
    return kStatus;
  }
  if (options->flags & fio::OpenFlags::kDescribe) {
    fidl::Status status = fidl::WireSendEvent(server_end)
                              ->OnOpen(ZX_OK, fio::wire::NodeInfoDeprecated::WithService({}));
    if (!status.ok()) {
      return status.status();
    }
  }
  return vnode->ConnectService(server_end.TakeChannel());
}

}  // namespace

void FilesystemInfo::SetFsId(const zx::event& event) {
  zx::result koid = GetObjectKoid(event);
  fs_id = koid.is_ok() ? *koid : ZX_KOID_INVALID;
}

fuchsia_io::wire::FilesystemInfo FilesystemInfo::ToFidl() const {
  fuchsia_io::wire::FilesystemInfo out = {};

  out.total_bytes = total_bytes;
  out.used_bytes = used_bytes;
  out.total_nodes = total_nodes;
  out.used_nodes = used_nodes;
  out.free_shared_pool_bytes = free_shared_pool_bytes;
  out.fs_id = fs_id;
  out.block_size = block_size;
  out.max_filename_size = max_filename_size;
  out.fs_type = static_cast<uint32_t>(fs_type);

  ZX_DEBUG_ASSERT(name.size() < fuchsia_io::wire::kMaxFsNameBuffer);
  out.name[name.copy(reinterpret_cast<char*>(out.name.data()),
                     fuchsia_io::wire::kMaxFsNameBuffer - 1)] = '\0';

  return out;
}

FuchsiaVfs::FuchsiaVfs(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

FuchsiaVfs::~FuchsiaVfs() = default;

void FuchsiaVfs::SetDispatcher(async_dispatcher_t* dispatcher) {
  ZX_ASSERT_MSG(!dispatcher_,
                "FuchsiaVfs::SetDispatcher maybe only be called when dispatcher_ is not set.");
  dispatcher_ = dispatcher;
}

zx_status_t FuchsiaVfs::Unlink(fbl::RefPtr<Vnode> vndir, std::string_view name, bool must_be_dir) {
  if (zx_status_t s = Vfs::Unlink(vndir, name, must_be_dir); s != ZX_OK)
    return s;

  vndir->Notify(name, fio::wire::WatchEvent::kRemoved);
  return ZX_OK;
}

void FuchsiaVfs::TokenDiscard(zx::event ios_token) {
  std::lock_guard lock(vfs_lock_);
  if (ios_token) {
    // The token is cleared here to prevent the following race condition:
    // 1) Open
    // 2) GetToken
    // 3) Close + Release Vnode
    // 4) Use token handle to access defunct vnode (or a different vnode, if the memory for it is
    //    reallocated).
    //
    // By cleared the token cookie, any remaining handles to the event will be ignored by the
    // filesystem server.
    //
    // This is called from a destructor, so there isn't much we can do if this fails.
    zx::result koid = GetObjectKoid(ios_token);
    if (koid.is_ok()) {
      vnode_tokens_.erase(*koid);
    }
  }
}

zx_status_t FuchsiaVfs::VnodeToToken(fbl::RefPtr<Vnode> vn, zx::event* ios_token, zx::event* out) {
  std::lock_guard lock(vfs_lock_);
  if (ios_token->is_valid()) {
    // Token has already been set for this iostate
    if (zx_status_t status = ios_token->duplicate(ZX_RIGHTS_BASIC, out); status != ZX_OK) {
      return status;
    }
    return ZX_OK;
  }

  zx::event new_token;
  zx::event new_ios_token;
  if (zx_status_t status = zx::event::create(0, &new_ios_token); status != ZX_OK) {
    return status;
  }
  if (zx_status_t status = new_ios_token.duplicate(ZX_RIGHTS_BASIC, &new_token); status != ZX_OK) {
    return status;
  }
  zx::result koid = GetObjectKoid(new_ios_token);
  if (koid.is_error()) {
    return koid.error_value();
  }
  vnode_tokens_.insert(std::make_unique<VnodeToken>(*koid, std::move(vn)));
  *ios_token = std::move(new_ios_token);
  *out = std::move(new_token);
  return ZX_OK;
}

bool FuchsiaVfs::IsTokenAssociatedWithVnode(zx::event token) {
  std::lock_guard lock(vfs_lock_);
  return TokenToVnode(std::move(token), nullptr) == ZX_OK;
}

zx::result<bool> FuchsiaVfs::EnsureExists(fbl::RefPtr<Vnode> vndir, std::string_view path,
                                          fbl::RefPtr<Vnode>* out_vn,
                                          fs::VnodeConnectionOptions options, uint32_t mode,
                                          fuchsia_io::Rights parent_rights) {
  zx::result result = Vfs::EnsureExists(vndir, path, out_vn, options, mode, parent_rights);
  if (result.is_ok() && result.value()) {
    vndir->Notify(path, fio::wire::WatchEvent::kAdded);
  }
  return result;
}

zx_status_t FuchsiaVfs::TokenToVnode(zx::event token, fbl::RefPtr<Vnode>* out) {
  zx::result koid = GetObjectKoid(token);
  if (koid.is_error()) {
    return koid.error_value();
  }
  const auto& vnode_token = vnode_tokens_.find(*koid);
  if (vnode_token == vnode_tokens_.end()) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (out) {
    *out = vnode_token->get_vnode();
  }
  return ZX_OK;
}

zx_status_t FuchsiaVfs::Rename(zx::event token, fbl::RefPtr<Vnode> oldparent,
                               std::string_view oldStr, std::string_view newStr) {
  // Local filesystem
  bool old_must_be_dir;
  {
    zx::result result = TrimName(oldStr);
    if (result.is_error()) {
      return result.status_value();
    }
    old_must_be_dir = result.value();
    if (oldStr == ".") {
      return ZX_ERR_UNAVAILABLE;
    }
    if (oldStr == "..") {
      return ZX_ERR_INVALID_ARGS;
    }
  }
  bool new_must_be_dir;
  {
    zx::result result = TrimName(newStr);
    if (result.is_error()) {
      return result.status_value();
    }
    new_must_be_dir = result.value();
    if (newStr == "." || newStr == "..") {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  fbl::RefPtr<fs::Vnode> newparent;
  {
    std::lock_guard lock(vfs_lock_);
    if (ReadonlyLocked()) {
      return ZX_ERR_ACCESS_DENIED;
    }
    if (zx_status_t status = TokenToVnode(std::move(token), &newparent); status != ZX_OK) {
      return status;
    }

    if (zx_status_t status =
            oldparent->Rename(newparent, oldStr, newStr, old_must_be_dir, new_must_be_dir);
        status != ZX_OK) {
      return status;
    }
  }
  oldparent->Notify(oldStr, fio::wire::WatchEvent::kRemoved);
  newparent->Notify(newStr, fio::wire::WatchEvent::kAdded);
  return ZX_OK;
}

zx::result<FilesystemInfo> FuchsiaVfs::GetFilesystemInfo() {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

zx_status_t FuchsiaVfs::Link(zx::event token, fbl::RefPtr<Vnode> oldparent, std::string_view oldStr,
                             std::string_view newStr) {
  std::lock_guard lock(vfs_lock_);
  fbl::RefPtr<fs::Vnode> newparent;
  if (zx_status_t status = TokenToVnode(std::move(token), &newparent); status != ZX_OK) {
    return status;
  }
  // Local filesystem
  if (ReadonlyLocked()) {
    return ZX_ERR_ACCESS_DENIED;
  }
  {
    zx::result result = TrimName(oldStr);
    if (result.is_error()) {
      return result.status_value();
    }
    if (result.value()) {
      return ZX_ERR_NOT_DIR;
    }
    if (oldStr == ".") {
      return ZX_ERR_UNAVAILABLE;
    }
    if (oldStr == "..") {
      return ZX_ERR_INVALID_ARGS;
    }
  }
  {
    zx::result result = TrimName(newStr);
    if (result.is_error()) {
      return result.status_value();
    }
    if (result.value()) {
      return ZX_ERR_NOT_DIR;
    }
    if (newStr == "." || newStr == "..") {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  // Look up the target vnode
  fbl::RefPtr<Vnode> target;
  if (zx_status_t status = oldparent->Lookup(oldStr, &target); status != ZX_OK) {
    return status;
  }
  if (zx_status_t status = newparent->Link(newStr, target); status != ZX_OK) {
    return status;
  }
  newparent->Notify(newStr, fio::wire::WatchEvent::kAdded);
  return ZX_OK;
}

zx_status_t FuchsiaVfs::Serve(fbl::RefPtr<Vnode> vnode, zx::channel server_end,
                              VnodeConnectionOptions options) {
  zx::result result = vnode->ValidateOptions(options);
  if (result.is_error()) {
    return result.status_value();
  }
  return Serve(std::move(vnode), std::move(server_end), result.value());
}

zx_status_t FuchsiaVfs::Serve(fbl::RefPtr<Vnode> vnode, zx::channel server_end,
                              Vnode::ValidatedOptions options) {
  VnodeProtocol protocol;
  if (options->flags & fio::OpenFlags::kNodeReference) {
    protocol = VnodeProtocol::kNode;
  } else {
    zx::result negotiated = NegotiateProtocol(options->protocols() & vnode->GetProtocols());
    if (negotiated.is_error()) {
      return negotiated.error_value();
    }
    protocol = *negotiated;
  }
  fidl::ServerEnd<fuchsia_io::Node> node(std::move(server_end));
  std::unique_ptr<internal::Connection> connection;

  switch (protocol) {
    case VnodeProtocol::kFile: {
      zx::result koid = GetObjectKoid(node.channel());
      if (koid.is_error()) {
        return koid.error_value();
      }
      zx::result<zx::stream> stream = vnode->CreateStream(ToStreamOptions(*options));
      if (stream.is_ok()) {
        connection = std::make_unique<internal::StreamFileConnection>(
            this, std::move(vnode), std::move(*stream), protocol, *options, *koid);
        break;
      }
      if (stream.error_value() != ZX_ERR_NOT_SUPPORTED) {
        return stream.error_value();
      }
      connection = std::make_unique<internal::RemoteFileConnection>(this, std::move(vnode),
                                                                    protocol, *options, *koid);
      break;
    }
    case VnodeProtocol::kDirectory: {
      zx::result koid = GetObjectKoid(node.channel());
      if (koid.is_error()) {
        return koid.error_value();
      }
      connection = std::make_unique<internal::DirectoryConnection>(this, std::move(vnode), protocol,
                                                                   *options, *koid);
      break;
    }
    case VnodeProtocol::kNode: {
      connection =
          std::make_unique<internal::NodeConnection>(this, std::move(vnode), protocol, *options);
      break;
    }
    case VnodeProtocol::kService: {
      return ConnectService(vnode, options, std::move(node));
    }
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
    case VnodeProtocol::kSymlink: {
      return ZX_ERR_NOT_SUPPORTED;
    }
#endif
  }

  // Send an |fuchsia.io/OnOpen| event if requested. At this point we know the connection is either
  // a Node connection, or a File/Directory that composes the node protocol.
  if (options->flags & fuchsia_io::OpenFlags::kDescribe) {
    zx::result representation = connection->NodeGetRepresentation();
    if (representation.is_error()) {
      // Ignore errors since there is nothing we can do if this fails.
      [[maybe_unused]] fidl::Status status =
          fidl::WireSendEvent(node)->OnOpen(representation.status_value(), {});
      return representation.status_value();
    }

    fs::HandleAsNodeInfoDeprecated(*std::move(representation), [&node](auto info) {
      // We ignore any errors sending the OnOpen event. Even if there are errors below, it's
      // possible that a client may have already sent messages into the channel and closed their end
      // without processing the response.
      [[maybe_unused]] fidl::Status status =
          fidl::WireSendEvent(node)->OnOpen(ZX_OK, std::move(info));
    });
  }

  return RegisterConnection(std::move(connection), node.TakeChannel());
}

zx_status_t FuchsiaVfs::ServeDirectory(fbl::RefPtr<fs::Vnode> vn,
                                       fidl::ServerEnd<fuchsia_io::Directory> server_end,
                                       fuchsia_io::Rights rights) {
  VnodeConnectionOptions options;
  options.flags |= fuchsia_io::OpenFlags::kDirectory;
  options.rights = rights;
  zx::result validated_options = vn->ValidateOptions(options);
  if (validated_options.is_error()) {
    return validated_options.status_value();
  }
  if (zx_status_t status = OpenVnode(validated_options.value(), &vn); status != ZX_OK) {
    return status;
  }

  return Serve(std::move(vn), server_end.TakeChannel(), validated_options.value());
}

}  // namespace fs
