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

zx_koid_t GetTokenKoid(const zx::event& token) {
  zx_info_handle_basic_t info = {};
  token.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return info.koid;
}

uint32_t ToStreamOptions(const VnodeConnectionOptions& options) {
  uint32_t stream_options = 0u;
  if (options.rights.read) {
    stream_options |= ZX_STREAM_MODE_READ;
  }
  if (options.rights.write) {
    stream_options |= ZX_STREAM_MODE_WRITE;
  }
  if (options.flags.append) {
    stream_options |= ZX_STREAM_MODE_APPEND;
  }
  return stream_options;
}

}  // namespace

void FilesystemInfo::SetFsId(const zx::event& event) {
  zx_info_handle_basic_t handle_info;
  if (zx_status_t status =
          event.get_info(ZX_INFO_HANDLE_BASIC, &handle_info, sizeof(handle_info), nullptr, nullptr);
      status == ZX_OK) {
    fs_id = handle_info.koid;
  } else {
    fs_id = ZX_KOID_INVALID;
  }
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
    auto rename_request = vnode_tokens_.erase(GetTokenKoid(ios_token));
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
  auto koid = GetTokenKoid(new_ios_token);
  vnode_tokens_.insert(std::make_unique<VnodeToken>(koid, std::move(vn)));
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
                                          Rights parent_rights) {
  zx::result result = Vfs::EnsureExists(vndir, path, out_vn, options, mode, parent_rights);
  if (result.is_ok() && result.value()) {
    vndir->Notify(path, fio::wire::WatchEvent::kAdded);
  }
  return result;
}

zx_status_t FuchsiaVfs::TokenToVnode(zx::event token, fbl::RefPtr<Vnode>* out) {
  const auto& vnode_token = vnode_tokens_.find(GetTokenKoid(token));
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
  if (!options->flags.node_reference) {
    auto candidate_protocols = options->protocols() & vnode->GetProtocols();
    ZX_DEBUG_ASSERT(
        candidate_protocols);  // |options| should ensure at least one supported protocol
    protocol = vnode->Negotiate(candidate_protocols);
  } else {
    // If |node_reference| is specified, serve |fuchsia.io/Node| regardless of the desired protocol.
    protocol = VnodeProtocol::kNode;
  }
  fidl::ServerEnd<fuchsia_io::Node> node(std::move(server_end));

  if (protocol == VnodeProtocol::kService) {
    // *NOTE*: This special handling is required to maintain io1 compatibility.
    //
    // For service connections, we don't know the final protocol that will be spoken over
    // |node| as it is dependent on the service Vnode type/connector. However, if |options.describe|
    // is set, the channel first speaks |fuchsia.io/Node| to send the OnOpen event, then switches to
    // the service protocol.
    //
    // However, we are not consistent with the use of epitaphs for Open implementations, nor does
    // io1 specify that they should be sent. However, but doing so provides better error
    // reporting to clients, and. This is also the behavior of the Rust VFS.
    //
    // TODO(https://fxbug.dev/324111653): In io2/Open2, service connections do not support the
    // OnRepresentation event, and use of epitaphs when closing channels is explicitly defined.
    // As such, we can remove this special handling and consolidate it with other node types when
    // we remove support for io1/Open.
    if (options->ToIoV1Flags() & ~(fio::OpenFlags::kDescribe | fio::OpenFlags::kNotDirectory)) {
      constexpr zx_status_t kStatus = ZX_ERR_INVALID_ARGS;
      if (options->flags.describe) {
        [[maybe_unused]] fidl::Status status = fidl::WireSendEvent(node)->OnOpen(kStatus, {});
      } else {
        [[maybe_unused]] zx_status_t status = node.Close(kStatus);
      }
      return kStatus;
    }
    if (options->flags.describe) {
      fidl::Status status =
          fidl::WireSendEvent(node)->OnOpen(ZX_OK, fio::wire::NodeInfoDeprecated::WithService({}));
      if (!status.ok()) {
        return status.status();
      }
    }
  }

  std::unique_ptr<internal::Connection> connection;
  zx_status_t status = ([&] {
    zx_info_handle_basic_t info;
    if (zx_status_t status =
            node.channel().get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
        status != ZX_OK) {
      return status;
    }
    switch (protocol) {
      case VnodeProtocol::kFile: {
        zx::result<zx::stream> stream = vnode->CreateStream(ToStreamOptions(*options));
        if (stream.is_ok()) {
          connection = std::make_unique<internal::StreamFileConnection>(
              this, std::move(vnode), std::move(*stream), protocol, *options, info.koid);
          return ZX_OK;
        }
        if (stream.error_value() != ZX_ERR_NOT_SUPPORTED) {
          return stream.error_value();
        }
        connection = std::make_unique<internal::RemoteFileConnection>(
            this, std::move(vnode), protocol, *options, info.koid);
        return ZX_OK;
      }
      case VnodeProtocol::kDirectory:
        connection = std::make_unique<internal::DirectoryConnection>(this, std::move(vnode),
                                                                     protocol, *options, info.koid);
        return ZX_OK;
      case VnodeProtocol::kNode:
        connection =
            std::make_unique<internal::NodeConnection>(this, std::move(vnode), protocol, *options);
        return ZX_OK;
      case VnodeProtocol::kService:
        return vnode->ConnectService(node.TakeChannel());
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
      case VnodeProtocol::kSymlink:
        return ZX_ERR_NOT_SUPPORTED;
#endif
    }
  }());
  if (status != ZX_OK || protocol == VnodeProtocol::kService) {
    return status;
  }

  // Send an |fuchsia.io/OnOpen| event if requested. At this point we know the connection is either
  // a Node connection, or a File/Directory that composes the node protocol.
  if (options->flags.describe) {
    fidl::Arena arena;
    zx::result<fio::wire::NodeInfoDeprecated> info;
    {
      zx::result<fuchsia_io::Representation> result = connection->NodeGetRepresentation();
      if (result.is_ok()) {
        info = zx::ok(ConvertToIoV1NodeInfo(arena, std::move(*result)));
      } else {
        info = result.take_error();
      }
    }
    if (info.is_error()) {
      // Ignore errors since there is nothing we can do if this fails.
      [[maybe_unused]] fidl::Status status =
          fidl::WireSendEvent(node)->OnOpen(info.status_value(), fio::wire::NodeInfoDeprecated());
      return info.status_value();
    }
    // We ignore the error and continue here in case the far end has queued open requests and
    // immediately closed the connection.  If the caller is doing that, they shouldn't have used
    // the describe flag, but there have been cases where this happened in the past and so we
    // preserve that behaviour for now.
    [[maybe_unused]] fidl::Status status =
        fidl::WireSendEvent(node)->OnOpen(ZX_OK, std::move(*info));
  }

  return RegisterConnection(std::move(connection), node.TakeChannel());
}

zx_status_t FuchsiaVfs::ServeDirectory(fbl::RefPtr<fs::Vnode> vn,
                                       fidl::ServerEnd<fuchsia_io::Directory> server_end,
                                       Rights rights) {
  VnodeConnectionOptions options;
  options.flags.directory = true;
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
