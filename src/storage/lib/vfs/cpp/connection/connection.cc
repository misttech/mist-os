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
  if (auto vfs = vfs_.Upgrade(); vfs && token_) {
    vfs->TokenDiscard(std::move(token_));
  }
}

void Connection::NodeCloneDeprecated(fio::OpenFlags flags, VnodeProtocol protocol,
                                     fidl::ServerEnd<fio::Node> server_end) {
  zx_status_t status = [&]() -> zx_status_t {
    zx::result clone_options = VnodeConnectionOptions::FromCloneFlags(flags, protocol);
    if (clone_options.is_error()) {
      FS_PRETTY_TRACE_DEBUG("[NodeCloneDeprecated] invalid clone flags: ", flags);
      return clone_options.error_value();
    }
    FS_PRETTY_TRACE_DEBUG("[NodeCloneDeprecated] our rights: ", rights(),
                          ", options: ", *clone_options);

    // If CLONE_SAME_RIGHTS is requested, cloned connection will inherit the same rights as those
    // from the originating connection.
    if (clone_options->flags & fio::OpenFlags::kCloneSameRights) {
      clone_options->rights = rights_;
    } else if (clone_options->rights - rights_) {
      // Return ACCESS_DENIED if the client asked for a right the parent connection doesn't have.
      return ZX_ERR_ACCESS_DENIED;
    }

    if (zx::result validated = vnode()->ValidateOptions(*clone_options); validated.is_error()) {
      return validated.error_value();
    }

    auto vfs = vfs_.Upgrade();
    if (!vfs)
      return ZX_ERR_CANCELED;

    fbl::RefPtr vn = vnode();
    // We only need to open the Vnode for non-node reference connection.
    if (protocol != VnodeProtocol::kNode) {
      if (zx_status_t open_status = OpenVnode(&vn); open_status != ZX_OK) {
        return open_status;
      }
    }
    // On failure, |Vfs::ServeDeprecated()| will close the channel with an epitaph.
    vfs->ServeDeprecated(vn, server_end.TakeChannel(), *clone_options);
    return ZX_OK;
  }();

  if (status != ZX_OK) {
    FS_PRETTY_TRACE_DEBUG("[NodeCloneDeprecated] error: ", zx_status_get_string(status));
    if (flags & fio::wire::OpenFlags::kDescribe) {
      // Ignore errors since there is nothing we can do if this fails.
      [[maybe_unused]] auto result = fidl::WireSendEvent(server_end)->OnOpen(status, {});
    }
    server_end.Close(status);
  }
}

void Connection::NodeClone(fio::Flags flags, zx::channel object) const {
  FS_PRETTY_TRACE_DEBUG("[NodeClone] reopening with flags: ", flags);
  auto vfs = this->vfs();
  if (!vfs) {
    fidl::ServerEnd<fio::Node>{std::move(object)}.Close(ZX_ERR_CANCELED);
    return;
  }
  // On failure, |Vfs::Serve()| will close the channel with an epitaph.
  [[maybe_unused]] zx_status_t status = vfs->Serve(vnode(), std::move(object), flags);
#if FS_TRACE_DEBUG_ENABLED
  if (status != ZX_OK) {
    FS_PRETTY_TRACE_DEBUG("[NodeClone] serve failed: ", zx_status_get_string(status));
  }
#endif  // FS_TRACE_DEBUG_ENABLED
}

zx::result<> Connection::NodeUpdateAttributes(const VnodeAttributesUpdate& update) {
  FS_PRETTY_TRACE_DEBUG("[NodeSetAttr] our rights: ", rights(),
                        ", setting attributes: ", update.Query(),
                        ", supported attributes: ", vnode_->SupportedMutableAttributes());
  if (!(rights_ & fio::Rights::kUpdateAttributes)) {
    return zx::error(ZX_ERR_BAD_HANDLE);
  }
  // Check that the Vnode allows setting the attributes we are updating.
  if (update.Query() - vnode_->SupportedMutableAttributes()) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  return vnode_->UpdateAttributes(update);
}

zx::result<fio::wire::FilesystemInfo> Connection::NodeQueryFilesystem() const {
  auto vfs = vfs_.Upgrade();
  if (!vfs)
    return zx::error(ZX_ERR_CANCELED);
  zx::result<FilesystemInfo> info = vfs->GetFilesystemInfo();
  if (info.is_error()) {
    return info.take_error();
  }
  return zx::ok(info.value().ToFidl());
}

namespace {
// Helper function to reduce verbosity in |NodeAttributes:Build| impl below by using type deduction.
// Returns an external (non-owning) |fidl::ObjectView| to |obj|.
template <typename T>
fidl::ObjectView<T> ExternalView(T* obj) {
  return fidl::ObjectView<T>::FromExternal(obj);
}
}  // namespace

zx::result<fio::wire::NodeAttributes2*> NodeAttributeBuilder::Build(
    fuchsia_io::NodeAttributesQuery query) {
  if (attributes_.is_error()) {
    return zx::error(attributes_.error_value());
  }
  fs::VnodeAttributes* attributes = &attributes_.value();
  // Immutable attributes:
  auto immutable_builder = ImmutableAttrs::ExternalBuilder(ExternalView(&immutable_frame_));
  if (query & fio::NodeAttributesQuery::kProtocols) {
    immutable_builder.protocols(ExternalView(&protocols_));
  }
  if (query & fio::NodeAttributesQuery::kAbilities) {
    immutable_builder.abilities(ExternalView(&abilities_));
  }
  if (query & fio::NodeAttributesQuery::kContentSize && attributes->content_size) {
    immutable_builder.content_size(ExternalView(&*attributes->content_size));
  }
  if (query & fio::NodeAttributesQuery::kStorageSize && attributes->storage_size) {
    immutable_builder.storage_size(ExternalView(&*attributes->storage_size));
  }
  if (query & fio::NodeAttributesQuery::kLinkCount && attributes->link_count) {
    immutable_builder.link_count(ExternalView(&*attributes->link_count));
  }
  if (query & fio::NodeAttributesQuery::kId && attributes->id) {
    immutable_builder.id(ExternalView(&*attributes->id));
  }
  // Mutable attributes:
  auto mutable_builder = MutableAttrs::ExternalBuilder(ExternalView(&mutable_frame_));
  if (query & fio::NodeAttributesQuery::kCreationTime && attributes->creation_time) {
    mutable_builder.creation_time(ExternalView(&*attributes->creation_time));
  }
  if (query & fio::NodeAttributesQuery::kModificationTime && attributes->modification_time) {
    mutable_builder.modification_time(ExternalView(&*attributes->modification_time));
  }
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(18)
  if (query & fio::NodeAttributesQuery::kMode && attributes->mode) {
    mutable_builder.mode(*attributes->mode);
  }
  if (query & fio::NodeAttributesQuery::kUid && attributes->uid) {
    mutable_builder.uid(*attributes->uid);
  }
  if (query & fio::NodeAttributesQuery::kGid && attributes->gid) {
    mutable_builder.gid(*attributes->gid);
  }
  if (query & fio::NodeAttributesQuery::kRdev && attributes->rdev) {
    mutable_builder.rdev(ExternalView(&*attributes->rdev));
  }
#endif

  // Build the wire table, which is now valid as long as this object remains in scope.
  wire_table_ = NodeAttributes2{
      .mutable_attributes = mutable_builder.Build(),
      .immutable_attributes = immutable_builder.Build(),
  };
  return zx::ok(&wire_table_);
}

}  // namespace fs::internal
