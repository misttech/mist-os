// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/connection/connection.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fidl/txn_header.h>
#include <lib/zx/handle.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <zircon/assert.h>

#include <memory>
#include <utility>

#include <fbl/string_buffer.h>

#include "src/storage/lib/vfs/cpp/debug.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

static_assert(fio::wire::kOpenFlagsAllowedWithNodeReference ==
                  (fio::wire::OpenFlags::kDirectory | fio::wire::OpenFlags::kNotDirectory |
                   fio::wire::OpenFlags::kDescribe | fio::wire::OpenFlags::kNodeReference),
              "OPEN_FLAGS_ALLOWED_WITH_NODE_REFERENCE value mismatch");
static_assert(PATH_MAX == fio::wire::kMaxPathLength + 1,
              "POSIX PATH_MAX inconsistent with Fuchsia MAX_PATH_LENGTH");
static_assert(NAME_MAX == fio::wire::kMaxFilename,
              "POSIX NAME_MAX inconsistent with Fuchsia MAX_FILENAME");

namespace fs::internal {

Connection::Connection(FuchsiaVfs* vfs, fbl::RefPtr<Vnode> vnode, fuchsia_io::Rights rights)
    : vfs_(vfs), vnode_(std::move(vnode)), rights_(rights) {
  ZX_DEBUG_ASSERT(vfs);
  ZX_DEBUG_ASSERT(vnode_);
}

Connection::~Connection() {
  // Release the token associated with this connection's vnode since the connection will be
  // releasing the vnode's reference once this function returns.
  if (token_) {
    vfs_->TokenDiscard(std::move(token_));
  }
}

void Connection::NodeClone(fio::OpenFlags flags, VnodeProtocol protocol,
                           fidl::ServerEnd<fio::Node> server_end) {
  auto clone_result = [=]() -> zx::result<std::tuple<fbl::RefPtr<Vnode>, VnodeConnectionOptions>> {
    zx::result clone_options = VnodeConnectionOptions::FromCloneFlags(flags);
    if (clone_options.is_error()) {
      FS_PRETTY_TRACE_DEBUG("[NodeClone] invalid clone flags: ", flags);
      return clone_options.take_error();
    }
    FS_PRETTY_TRACE_DEBUG("[NodeClone] our rights: ", rights(), ", options: ", *clone_options);

    // If CLONE_SAME_RIGHTS is requested, cloned connection will inherit the same rights as those
    // from the originating connection.
    if (clone_options->flags & fio::OpenFlags::kCloneSameRights) {
      clone_options->rights = rights_;
    } else if (clone_options->rights - rights_) {
      // Return ACCESS_DENIED if the client asked for a right the parent connection doesn't have.
      return zx::error(ZX_ERR_ACCESS_DENIED);
    }

    // Ensure we map the request to the correct flags based on the connection's protocol.
    switch (protocol) {
      case fs::VnodeProtocol::kNode: {
        clone_options->flags |= fio::OpenFlags::kNodeReference;
        break;
      }
      case fs::VnodeProtocol::kDirectory: {
        clone_options->flags |= fio::OpenFlags::kDirectory;
        break;
      }
      default: {
        clone_options->flags |= fio::OpenFlags::kNotDirectory;
        break;
      }
    }

    if (zx::result validated = vnode()->ValidateOptions(*clone_options); validated.is_error()) {
      return validated.take_error();
    }
    fbl::RefPtr vn = vnode();
    if (protocol != VnodeProtocol::kNode) {
      // We only need to open the underlying Vnode again if this isn't a node reference connection.
      if (zx_status_t open_status = OpenVnode(&vn); open_status != ZX_OK) {
        return zx::error(open_status);
      }
    }

    return zx::ok(std::make_tuple(std::move(vn), *clone_options));
  }();

  if (clone_result.is_ok()) {
    auto [vnode, options] = *std::move(clone_result);
    vfs_->Serve(vnode, server_end.TakeChannel(), options);
  } else {
    FS_PRETTY_TRACE_DEBUG("[NodeClone] error: ", clone_result.status_string());
    if (flags & fio::OpenFlags::kDescribe) {
      // Ignore errors since there is nothing we can do if this fails.
      [[maybe_unused]] auto result =
          fidl::WireSendEvent(server_end)
              ->OnOpen(clone_result.error_value(), fio::wire::NodeInfoDeprecated());
    }
  }
}

zx::result<VnodeAttributes> Connection::NodeGetAttr() const {
  FS_PRETTY_TRACE_DEBUG("[NodeGetAttr] rights: ", rights());
  // TODO(https://fxbug.dev/324080764): This io1 operation should require the GET_ATTRIBUTES right.
  fs::VnodeAttributes attr;
  if (zx_status_t status = vnode_->GetAttributes(&attr); status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(attr);
}

zx::result<> Connection::NodeSetAttr(fio::NodeAttributeFlags flags,
                                     const fio::wire::NodeAttributes& attributes) {
  FS_PRETTY_TRACE_DEBUG("[NodeSetAttr] our rights: ", rights(), ", incoming flags: ", flags);
  if (!(rights_ & fio::Rights::kUpdateAttributes)) {
    return zx::error(ZX_ERR_BAD_HANDLE);
  }
  fs::VnodeAttributesUpdate update;
  if (flags & fio::NodeAttributeFlags::kCreationTime) {
    update.set_creation_time(attributes.creation_time);
  }
  if (flags & fio::NodeAttributeFlags::kModificationTime) {
    update.set_modification_time(attributes.modification_time);
  }
  return zx::make_result(vnode_->SetAttributes(update));
}

zx::result<fio::wire::FilesystemInfo> Connection::NodeQueryFilesystem() const {
  zx::result<FilesystemInfo> info = vfs_->GetFilesystemInfo();
  if (info.is_error()) {
    return info.take_error();
  }
  return zx::ok(info.value().ToFidl());
}

}  // namespace fs::internal
