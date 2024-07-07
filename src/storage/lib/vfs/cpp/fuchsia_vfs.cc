// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/fuchsia_vfs.h"

#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/zx/event.h>
#include <lib/zx/process.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/availability.h>

#include <memory>
#include <string_view>
#include <utility>

#include <fbl/auto_lock.h>
#include <fbl/ref_ptr.h>

#include "src/storage/lib/vfs/cpp/connection/connection.h"
#include "src/storage/lib/vfs/cpp/connection/directory_connection.h"
#include "src/storage/lib/vfs/cpp/connection/node_connection.h"
#include "src/storage/lib/vfs/cpp/connection/remote_file_connection.h"
#include "src/storage/lib/vfs/cpp/connection/stream_file_connection.h"
#include "src/storage/lib/vfs/cpp/debug.h"
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

uint32_t ToStreamOptions(fuchsia_io::Rights rights, bool append) {
  uint32_t stream_options = 0u;
  if (rights & fuchsia_io::Rights::kReadBytes) {
    stream_options |= ZX_STREAM_MODE_READ;
  }
  if (rights & fuchsia_io::Rights::kWriteBytes) {
    stream_options |= ZX_STREAM_MODE_WRITE;
  }
  if (append) {
    stream_options |= ZX_STREAM_MODE_APPEND;
  }
  return stream_options;
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

FuchsiaVfs::SharedPtr& FuchsiaVfs::SharedPtr::operator=(const FuchsiaVfs::SharedPtr& other) {
  if (this == &other)
    return *this;
  Reset();
  vfs_ = other.vfs_;
  if (vfs_)
    vfs_->ref_->strong_count.fetch_add(1);
  return *this;
}

void FuchsiaVfs::SharedPtr::Reset() {
  if (vfs_) {
    if (vfs_->ref_->strong_count.fetch_sub(1) == 1) {
      sync_completion_signal(&vfs_->done_);
      // And now drop the implicit weak reference.
      WeakPtr weak(vfs_->ref_);
    }
    vfs_ = nullptr;
  }
}

FuchsiaVfs::SharedPtr FuchsiaVfs::WeakPtr::Upgrade() const {
  int current = ref_->strong_count.load();
  while (current > 0) {
    if (ref_->strong_count.compare_exchange_weak(current, current + 1)) {
      return SharedPtr(ref_->vfs);
    }
  }
  return SharedPtr(nullptr);
}

FuchsiaVfs::FuchsiaVfs(async_dispatcher_t* dispatcher)
    // We start with one strong count and one weak count.  The strong count is returned in
    // `WillDestroy` and the weak count is returned when the last strong count is returned.
    : dispatcher_(dispatcher), ref_(new Ref{.strong_count = 1, .weak_count = 1, .vfs = this}) {}

FuchsiaVfs::~FuchsiaVfs() {
  if (!is_terminating_)
    WillDestroy();
}

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

zx_status_t FuchsiaVfs::Serve(const fbl::RefPtr<Vnode>& vnode, zx::channel server_end,
                              VnodeConnectionOptions options) {
  zx_status_t status = ServeImpl(vnode, server_end, options);
  if (status != ZX_OK) {
    FS_PRETTY_TRACE_DEBUG("[FuchsiaVfs::Serve] Error: ", zx_status_get_string(status));
    if (!(options.flags & fuchsia_io::OpenFlags::kNodeReference)) {
      vnode->Close();  // Balance open count for non-node reference connections.
    }
    // If |server_end| wasn't consumed, send an event if required and close it with an epitaph.
    if (server_end.is_valid()) {
      fidl::ServerEnd<fio::Node> node{std::move(server_end)};
      if (options.flags & fio::wire::OpenFlags::kDescribe) {
        // Ignore errors since there is nothing we can do if this fails.
        [[maybe_unused]] auto result = fidl::WireSendEvent(node)->OnOpen(status, {});
      }
      // Close the channel with an epitaph indicating the failure mode.
      node.Close(status);
    }
  }
  return status;
}

zx_status_t FuchsiaVfs::ServeImpl(const fbl::RefPtr<Vnode>& vnode, zx::channel& server_end,
                                  VnodeConnectionOptions options) {
  if (zx::result result = vnode->ValidateOptions(options); result.is_error()) {
    return result.error_value();
  }
  // Determine the protocol we should use to serve |vnode| based on |options|.
  VnodeProtocol protocol;
  if (options.flags & fio::OpenFlags::kNodeReference) {
    protocol = VnodeProtocol::kNode;
  } else {
    zx::result negotiated = internal::NegotiateProtocol(vnode->GetProtocols(), options.protocols());
    if (negotiated.is_error()) {
      return negotiated.error_value();
    }
    protocol = *negotiated;
  }
  // Downscope the set of rights granted to this connection to only include those which are relevant
  // for the protocol which will be served.
  options.rights = internal::DownscopeRights(options.rights, protocol);

  std::unique_ptr<internal::Connection> connection;
  switch (protocol) {
    case VnodeProtocol::kFile: {
      zx::result koid = GetObjectKoid(server_end);
      if (koid.is_error()) {
        return koid.error_value();
      }
      bool append = static_cast<bool>(options.flags & fio::OpenFlags::kAppend);
      // Truncate was handled when the Vnode was opened, only append mode applies to this
      // connection. |ToStreamOptions| should handle setting the stream to append mode.
      zx::result<zx::stream> stream = vnode->CreateStream(ToStreamOptions(options.rights, append));
      if (stream.is_ok()) {
        connection = std::make_unique<internal::StreamFileConnection>(
            this, vnode, options.rights, append, std::move(*stream), *koid);
        break;
      }
      if (stream.error_value() != ZX_ERR_NOT_SUPPORTED) {
        return stream.error_value();
      }
      connection = std::make_unique<internal::RemoteFileConnection>(this, vnode, options.rights,
                                                                    append, *koid);
      break;
    }
    case VnodeProtocol::kDirectory: {
      zx::result koid = GetObjectKoid(server_end);
      if (koid.is_error()) {
        return koid.error_value();
      }
      connection =
          std::make_unique<internal::DirectoryConnection>(this, vnode, options.rights, *koid);
      break;
    }
    case VnodeProtocol::kNode: {
      connection = std::make_unique<internal::NodeConnection>(this, vnode, options.rights);
      break;
    }
    case VnodeProtocol::kService: {
      if (options.ToIoV1Flags() & ~(fio::OpenFlags::kDescribe | fio::OpenFlags::kNotDirectory)) {
        return ZX_ERR_INVALID_ARGS;
      }
      return vnode->ConnectService(std::move(server_end));
    }
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    case VnodeProtocol::kSymlink: {
      return ZX_ERR_NOT_SUPPORTED;
    }
#endif
  }

  // Send an |fuchsia.io/OnOpen| event if requested. At this point we know the connection is either
  // a Node connection, or a File/Directory that composes the node protocol.
  fidl::UnownedServerEnd<fio::Node> node{server_end.borrow()};
  if (options.flags & fuchsia_io::OpenFlags::kDescribe) {
    zx_status_t status =
        connection->WithNodeInfoDeprecated([&node](fio::wire::NodeInfoDeprecated info) {
          return fidl::WireSendEvent(node)->OnOpen(ZX_OK, std::move(info)).status();
        });
    if (status != ZX_OK) {
      return status;
    }
  }

  return RegisterConnection(std::move(connection), server_end).status_value();
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
  if (zx_status_t status = OpenVnode(&vn); status != ZX_OK) {
    return status;
  }

  return Serve(vn, server_end.TakeChannel(), options);
}

zx::result<> FuchsiaVfs::Serve2(Vfs::Open2Result open_result, fuchsia_io::Rights rights,
                                zx::channel object_request,
                                const fuchsia_io::wire::ConnectionProtocols* protocols) {
  zx::result result = Serve2Impl(std::move(open_result), rights, object_request, protocols);
  // On any errors, if the channel wasn't consumed, ensure we close it with an epitaph.
  if (result.is_error()) {
    FS_PRETTY_TRACE_DEBUG("[FuchsiaVfs::Serve2] Error: ", result.status_string());
    if (object_request.is_valid()) {
      fidl::ServerEnd<fio::Node> node{std::move(object_request)};
      node.Close(result.error_value());
    }
  }
  return result;
}

zx::result<> FuchsiaVfs::Serve2Impl(Vfs::Open2Result open_result, fuchsia_io::Rights rights,
                                    zx::channel& object_request,
                                    const fuchsia_io::wire::ConnectionProtocols* protocols) {
  const fio::wire::NodeOptions* node_options =
      protocols && protocols->is_node() ? &protocols->node() : nullptr;
  const fbl::RefPtr<fs::Vnode>& vnode = open_result.vnode();
  fs::VnodeProtocol protocol = open_result.protocol();
  // Downscope the rights granted to the connection to only include those for this protocol.
  rights = internal::DownscopeRights(rights, protocol);

  // Create the connection that will handle requests for this Vnode.
  std::unique_ptr<internal::Connection> connection;
  switch (protocol) {
    case fs::VnodeProtocol::kDirectory: {
      zx::result koid = GetObjectKoid(object_request);
      if (koid.is_error()) {
        return koid.take_error();
      }
      connection = std::make_unique<internal::DirectoryConnection>(this, vnode, rights, *koid);
      break;
    }
    case VnodeProtocol::kFile: {
      zx::result koid = GetObjectKoid(object_request);
      if (koid.is_error()) {
        return koid.take_error();
      }
      const fio::FileProtocolFlags* file_flags =
          node_options && node_options->has_protocols() && node_options->protocols().has_file()
              ? &node_options->protocols().file()
              : nullptr;
      bool append = file_flags && (*file_flags & fio::FileProtocolFlags::kAppend);
      // Truncate was handled when the Vnode was opened, only append mode applies to this
      // connection. |ToStreamOptions| should handle setting the stream to append mode.
      zx::result<zx::stream> stream = vnode->CreateStream(ToStreamOptions(rights, append));
      if (stream.is_ok()) {
        connection = std::make_unique<internal::StreamFileConnection>(this, vnode, rights, append,
                                                                      std::move(*stream), *koid);
        break;
      }
      if (stream.error_value() != ZX_ERR_NOT_SUPPORTED) {
        return stream.take_error();
      }
      connection =
          std::make_unique<internal::RemoteFileConnection>(this, vnode, rights, append, *koid);
      break;
    }
    case VnodeProtocol::kNode: {
      connection = std::make_unique<internal::NodeConnection>(this, vnode, rights);
      break;
    }
    case fs::VnodeProtocol::kService: {
      ZX_DEBUG_ASSERT(!protocols || protocols->is_connector());
      return zx::make_result(vnode->ConnectService(std::move(object_request)));
    }
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    case VnodeProtocol::kSymlink: {
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
#endif
  }
  ZX_DEBUG_ASSERT(connection != nullptr);
  // Send an OnRepresentation event if the request requires one.
  if (node_options && node_options->has_flags() &&
      (node_options->flags() & fuchsia_io::NodeFlags::kGetRepresentation)) {
    std::optional<fio::NodeAttributesQuery> attribute_query = std::nullopt;
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
    if (node_options->has_attributes()) {
      attribute_query = node_options->attributes();
    }
#endif
    zx::result get_representation = connection->WithRepresentation(
        [&](fio::wire::Representation representation) -> zx::result<> {
          return zx::make_result(
              fidl::WireSendEvent(fidl::UnownedServerEnd<fio::Node>{object_request.borrow()})
                  ->OnRepresentation(std::move(representation))
                  .status());
        },
        attribute_query);
    if (get_representation.is_error()) {
      return get_representation.take_error();
    }
  }

  // Register the connection with the VFS. On success, the connection is bound to the channel, and
  // starts servicing requests.
  zx::result result = RegisterConnection(std::move(connection), object_request);
  if (result.is_ok()) {
    open_result.TakeVnode();  // On success, the connection is responsible for closing the Vnode.
  }
  return result;
}

}  // namespace fs
