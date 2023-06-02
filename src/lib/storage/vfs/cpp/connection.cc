// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/storage/vfs/cpp/connection.h"

#include <fidl/fuchsia.hardware.pty/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fdio/io.h>
#include <lib/fdio/vfs.h>
#include <lib/fidl/txn_header.h>
#include <lib/zx/handle.h>
#include <limits.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <zircon/assert.h>

#include <iterator>
#include <memory>
#include <type_traits>
#include <utility>

#include <fbl/string_buffer.h>

#include "src/lib/storage/vfs/cpp/debug.h"
#include "src/lib/storage/vfs/cpp/vfs_types.h"
#include "src/lib/storage/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

static_assert(fio::wire::kOpenFlagsAllowedWithNodeReference ==
                  (fio::wire::OpenFlags::kDirectory | fio::wire::OpenFlags::kNotDirectory |
                   fio::wire::OpenFlags::kDescribe | fio::wire::OpenFlags::kNodeReference),
              "OPEN_FLAGS_ALLOWED_WITH_NODE_REFERENCE value mismatch");
static_assert(PATH_MAX == fio::wire::kMaxPathLength + 1,
              "POSIX PATH_MAX inconsistent with Fuchsia MAX_PATH_LENGTH");
static_assert(NAME_MAX == fio::wire::kMaxFilename,
              "POSIX NAME_MAX inconsistent with Fuchsia MAX_FILENAME");

namespace fs {

namespace internal {

bool PrevalidateFlags(fio::wire::OpenFlags flags) {
  if (flags & fio::wire::OpenFlags::kNodeReference) {
    // Explicitly reject VNODE_REF_ONLY together with any invalid flags.
    if (flags - fio::wire::kOpenFlagsAllowedWithNodeReference) {
      return false;
    }
  }

  if ((flags & fio::wire::OpenFlags::kNotDirectory) && (flags & fio::wire::OpenFlags::kDirectory)) {
    return false;
  }

  return true;
}

zx_status_t EnforceHierarchicalRights(Rights parent_rights, VnodeConnectionOptions child_options,
                                      VnodeConnectionOptions* out_options) {
  // The POSIX compatibiltiy flags allow the child directory connection to inherit the writable
  // and executable rights.  If there exists a directory without the corresponding right along
  // the Open() chain, we remove that POSIX flag preventing it from being inherited down the line
  // (this applies both for local and remote mount points, as the latter may be served using
  // a connection with vastly greater rights).
  if (child_options.flags.posix_write && !parent_rights.write) {
    child_options.flags.posix_write = false;
  }
  if (child_options.flags.posix_execute && !parent_rights.execute) {
    child_options.flags.posix_execute = false;
  }
  if (!child_options.rights.StricterOrSameAs(parent_rights)) {
    // Client asked for some right but we do not have it
    return ZX_ERR_ACCESS_DENIED;
  }
  *out_options = child_options;
  return ZX_OK;
}

Connection::Connection(FuchsiaVfs* vfs, fbl::RefPtr<Vnode> vnode, VnodeProtocol protocol,
                       VnodeConnectionOptions options)
    : vnode_is_open_(!options.flags.node_reference),
      vfs_(vfs),
      vnode_(std::move(vnode)),
      protocol_(protocol),
      options_(VnodeConnectionOptions::FilterForNewConnection(options)) {
  ZX_DEBUG_ASSERT(vfs);
  ZX_DEBUG_ASSERT(vnode_);
}

Connection::~Connection() {
  // Invoke a "close" call on the underlying vnode if we haven't already.
  EnsureVnodeClosed();

  // Release the token associated with this connection's vnode since the connection will be
  // releasing the vnode's reference once this function returns.
  if (token_) {
    vfs_->TokenDiscard(std::move(token_));
  }
}

void Connection::Unbind() { binding_.reset(); }

void Connection::StartDispatching(zx::channel channel, OnUnbound on_unbound) {
  ZX_DEBUG_ASSERT(channel);
  ZX_DEBUG_ASSERT(!binding_);
  ZX_DEBUG_ASSERT(vfs_->dispatcher());
  ZX_DEBUG_ASSERT_MSG(InContainer(),
                      "Connection must be managed by the Vfs when dispatching FIDL messages.");

  binding_ = Bind(vfs_->dispatcher(), std::move(channel), std::move(on_unbound));
}

zx_status_t Connection::EnsureVnodeClosed() {
  if (!vnode_is_open_) {
    return ZX_OK;
  }
  vnode_is_open_ = false;
  return vnode_->Close();
}

void Connection::NodeClone(fio::wire::OpenFlags flags, fidl::ServerEnd<fio::Node> server_end) {
  auto clone_options = VnodeConnectionOptions::FromIoV1Flags(flags);
  auto write_error = [describe = clone_options.flags.describe](fidl::ServerEnd<fio::Node> channel,
                                                               zx_status_t error) {
    FS_PRETTY_TRACE_DEBUG("[NodeClone] error: ", zx_status_get_string(error));
    if (describe) {
      // Ignore errors since there is nothing we can do if this fails.
      [[maybe_unused]] auto result =
          fidl::WireSendEvent(channel)->OnOpen(error, fio::wire::NodeInfoDeprecated());
      channel.reset();
    }
  };
  if (!PrevalidateFlags(flags)) {
    FS_PRETTY_TRACE_DEBUG("[NodeClone] prevalidate failed", ", incoming flags: ", flags);
    return write_error(std::move(server_end), ZX_ERR_INVALID_ARGS);
  }
  FS_PRETTY_TRACE_DEBUG("[NodeClone] our options: ", options(),
                        ", incoming options: ", clone_options);

  // If CLONE_SAME_RIGHTS is specified, the client cannot request any specific rights.
  if (clone_options.flags.clone_same_rights && clone_options.rights.any()) {
    return write_error(std::move(server_end), ZX_ERR_INVALID_ARGS);
  }
  // These two flags are always preserved.
  clone_options.flags.append = options().flags.append;
  clone_options.flags.node_reference = options().flags.node_reference;
  // If CLONE_SAME_RIGHTS is requested, cloned connection will inherit the same rights as those from
  // the originating connection.
  if (clone_options.flags.clone_same_rights) {
    clone_options.rights = options().rights;
  }
  if (!clone_options.rights.StricterOrSameAs(options().rights)) {
    return write_error(std::move(server_end), ZX_ERR_ACCESS_DENIED);
  }

  fbl::RefPtr<Vnode> vn(vnode_);
  auto result = vn->ValidateOptions(clone_options);
  if (result.is_error()) {
    return write_error(std::move(server_end), result.status_value());
  }
  auto& validated_options = result.value();
  zx_status_t open_status = ZX_OK;
  if (!clone_options.flags.node_reference) {
    open_status = OpenVnode(validated_options, &vn);
  }
  if (open_status != ZX_OK) {
    return write_error(std::move(server_end), open_status);
  }

  vfs_->Serve(vn, server_end.TakeChannel(), validated_options);
}

zx::result<> Connection::NodeClose() {
  Unbind();
  return zx::make_result(EnsureVnodeClosed());
}

fidl::VectorView<uint8_t> Connection::NodeQuery() {
  const std::string_view kProtocol = [this]() {
    if (options().flags.node_reference) {
      return fio::wire::kNodeProtocolName;
    }
    switch (protocol()) {
      case VnodeProtocol::kConnector: {
        return fio::wire::kNodeProtocolName;
      }
      case VnodeProtocol::kFile: {
        return fio::wire::kFileProtocolName;
      }
      case VnodeProtocol::kDirectory: {
        return fio::wire::kDirectoryProtocolName;
      }
    }
  }();
  // TODO(https://fxbug.dev/101890): avoid the const cast.
  uint8_t* data = reinterpret_cast<uint8_t*>(const_cast<char*>(kProtocol.data()));
  return fidl::VectorView<uint8_t>::FromExternal(data, kProtocol.size());
}

zx::result<VnodeRepresentation> Connection::NodeDescribe() {
  if (options().flags.node_reference) {
    return zx::ok(VnodeRepresentation::Connector());
  }
  fs::VnodeRepresentation representation;
  zx_status_t status =
      vnode()->GetNodeInfoForProtocol(protocol(), options().rights, &representation);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(representation));
}

void Connection::NodeSync(fit::callback<void(zx_status_t)> callback) {
  FS_PRETTY_TRACE_DEBUG("[NodeSync] options: ", options());

  if (options().flags.node_reference) {
    return callback(ZX_ERR_BAD_HANDLE);
  }
  vnode_->Sync(Vnode::SyncCallback(std::move(callback)));
}

zx::result<VnodeAttributes> Connection::NodeGetAttr() {
  FS_PRETTY_TRACE_DEBUG("[NodeGetAttr] options: ", options());

  fs::VnodeAttributes attr;
  if (zx_status_t status = vnode_->GetAttributes(&attr); status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(attr);
}

zx::result<> Connection::NodeSetAttr(fio::wire::NodeAttributeFlags flags,
                                     const fio::wire::NodeAttributes& attributes) {
  FS_PRETTY_TRACE_DEBUG("[NodeSetAttr] our options: ", options(), ", incoming flags: ", flags);

  if (options().flags.node_reference) {
    return zx::error(ZX_ERR_BAD_HANDLE);
  }
  if (!options().rights.write) {
    return zx::error(ZX_ERR_BAD_HANDLE);
  }

  fs::VnodeAttributesUpdate update;
  if (flags & fio::wire::NodeAttributeFlags::kCreationTime) {
    update.set_creation_time(attributes.creation_time);
  }
  if (flags & fio::wire::NodeAttributeFlags::kModificationTime) {
    update.set_modification_time(attributes.modification_time);
  }
  return zx::make_result(vnode_->SetAttributes(update));
}

zx::result<fio::wire::OpenFlags> Connection::NodeGetFlags() {
  return zx::ok(options().ToIoV1Flags() & (kStatusFlags | fio::wire::kOpenRights));
}

zx::result<> Connection::NodeSetFlags(fio::wire::OpenFlags flags) {
  if (options().flags.node_reference) {
    return zx::error(ZX_ERR_BAD_HANDLE);
  }
  auto options = VnodeConnectionOptions::FromIoV1Flags(flags);
  set_append(options.flags.append);
  return zx::ok();
}

zx::result<fuchsia_io::wire::FilesystemInfo> Connection::NodeQueryFilesystem() {
  zx::result<FilesystemInfo> info = vfs_->GetFilesystemInfo();
  if (info.is_error()) {
    return info.take_error();
  }
  return zx::ok(info.value().ToFidl());
}

}  // namespace internal

}  // namespace fs
