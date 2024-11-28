// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/vfs_types.h"

#include <fidl/fuchsia.io/cpp/natural_types.h>
#include <fidl/fuchsia.io/cpp/wire_types.h>
#include <lib/fit/function.h>

#include "src/storage/lib/vfs/cpp/vnode.h"

namespace fio = fuchsia_io;

// Verify that permission flags align with the Rights enumeration.
static_assert(static_cast<uint64_t>(fio::Rights::kConnect) ==
              static_cast<uint64_t>(fio::Flags::kPermConnect));
static_assert(static_cast<uint64_t>(fio::Rights::kReadBytes) ==
              static_cast<uint64_t>(fio::Flags::kPermRead));
static_assert(static_cast<uint64_t>(fio::Rights::kWriteBytes) ==
              static_cast<uint64_t>(fio::Flags::kPermWrite));
static_assert(static_cast<uint64_t>(fio::Rights::kExecute) ==
              static_cast<uint64_t>(fio::Flags::kPermExecute));
static_assert(static_cast<uint64_t>(fio::Rights::kGetAttributes) ==
              static_cast<uint64_t>(fio::Flags::kPermGetAttributes));
static_assert(static_cast<uint64_t>(fio::Rights::kUpdateAttributes) ==
              static_cast<uint64_t>(fio::Flags::kPermSetAttributes));
static_assert(static_cast<uint64_t>(fio::Rights::kEnumerate) ==
              static_cast<uint64_t>(fio::Flags::kPermEnumerate));
static_assert(static_cast<uint64_t>(fio::Rights::kTraverse) ==
              static_cast<uint64_t>(fio::Flags::kPermTraverse));
static_assert(static_cast<uint64_t>(fio::Rights::kModifyDirectory) ==
              static_cast<uint64_t>(fio::Flags::kPermModify));

namespace fs {

namespace {

constexpr VnodeConnectionOptions FlagsToConnectionOptions(fio::OpenFlags flags) {
  VnodeConnectionOptions options;
  // Filter out io1 OpenFlags.RIGHT_* flags, translated to io2 Rights below.
  options.flags = flags & ~kAllIo1Rights;

  // Using Open1 requires GET_ATTRIBUTES as this is not expressible via |fio::OpenFlags|.
  // TODO(https://fxbug.dev/324080764): Restrict GET_ATTRIBUTES.
  options.rights = fio::Rights::kGetAttributes;

  // Approximate a set of io2 Rights corresponding to what is expected by |flags|.
  if (!(options.flags & fio::OpenFlags::kNodeReference)) {
    if (flags & fio::OpenFlags::kRightReadable) {
      options.rights |= fio::kRStarDir;
    }
    if (flags & fio::OpenFlags::kRightWritable) {
      options.rights |= fio::kWStarDir;
    }
    if (flags & fio::OpenFlags::kRightExecutable) {
      options.rights |= fio::kXStarDir;
    }
  }

  return options;
}

}  // namespace

zx::result<VnodeConnectionOptions> VnodeConnectionOptions::FromOpen1Flags(fio::OpenFlags flags) {
  if ((flags & fio::OpenFlags::kNodeReference) &&
      (flags - fio::kOpenFlagsAllowedWithNodeReference)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if ((flags & fio::OpenFlags::kNotDirectory) && (flags & fio::OpenFlags::kDirectory)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (flags & fio::OpenFlags::kCloneSameRights) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if ((flags & fio::OpenFlags::kTruncate) && !(flags & fio::OpenFlags::kRightWritable)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(FlagsToConnectionOptions(flags));
}

zx::result<VnodeConnectionOptions> VnodeConnectionOptions::FromCloneFlags(fio::OpenFlags flags,
                                                                          VnodeProtocol protocol) {
  constexpr fio::OpenFlags kValidCloneFlags = kAllIo1Rights | fio::OpenFlags::kAppend |
                                              fio::OpenFlags::kDescribe |
                                              fio::OpenFlags::kCloneSameRights;
  // Any flags not present in |kValidCloneFlags| should be ignored.
  flags &= kValidCloneFlags;

  // If CLONE_SAME_RIGHTS is specified, the client cannot request any specific rights.
  if ((flags & fio::OpenFlags::kCloneSameRights) && (flags & kAllIo1Rights)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Ensure we map the request to the correct flags based on the connection's protocol.
  switch (protocol) {
    case fs::VnodeProtocol::kNode: {
      flags |= fio::OpenFlags::kNodeReference;
      break;
    }
    case fs::VnodeProtocol::kDirectory: {
      flags |= fio::OpenFlags::kDirectory;
      break;
    }
    default: {
      flags |= fio::OpenFlags::kNotDirectory;
      break;
    }
  }

  VnodeConnectionOptions options = FlagsToConnectionOptions(flags);

  // Downscope the rights specified by |flags| to match those that were granted to this node
  // based on |protocol|. io1 OpenFlags expand to a set of rights which may not be compatible
  // with this protocol (e.g. OpenFlags::RIGHT_WRITABLE grants Rights::MODIFY_DIRECTORY, but this is
  // not applicable to files which do not have this right).
  options.rights = internal::DownscopeRights(options.rights, protocol);

  return zx::ok(options);
}

fio::OpenFlags VnodeConnectionOptions::ToIoV1Flags() const {
  return flags | RightsToOpenFlags(rights);
}

fio::OpenFlags RightsToOpenFlags(fio::Rights rights) {
  fio::OpenFlags flags = {};
  // Map io2 rights to io1 flags only if all constituent io2 rights are present.
  if ((rights & fio::kRStarDir) == fio::kRStarDir) {
    flags |= fio::OpenFlags::kRightReadable;
  }
  if ((rights & fio::kWStarDir) == fio::kWStarDir) {
    flags |= fio::OpenFlags::kRightWritable;
  }
  if ((rights & fio::kXStarDir) == fio::kXStarDir) {
    flags |= fio::OpenFlags::kRightExecutable;
  }
  return flags;
}

fio::wire::NodeAttributes VnodeAttributes::ToIoV1NodeAttributes(const fs::Vnode& vnode) const {
  // Filesystems that don't support hard links typically report a value of 1 for the link count.
  constexpr uint64_t kDefaultLinkCount = 1;
  return fio::wire::NodeAttributes{
      .mode = mode.has_value() ? *mode
                               : internal::GetPosixMode(vnode.GetProtocols(), vnode.GetAbilities()),
      .id = id.value_or(fio::wire::kInoUnknown),
      .content_size = content_size.value_or(0),
      .storage_size = storage_size.value_or(0),
      .link_count = link_count.value_or(kDefaultLinkCount),
      .creation_time = creation_time.value_or(0),
      .modification_time = modification_time.value_or(0)};
}

namespace internal {

fio::Rights DownscopeRights(fio::Rights rights, VnodeProtocol protocol) {
  switch (protocol) {
    case VnodeProtocol::kDirectory: {
      // Directories support all rights.
      return rights;
    }
    case VnodeProtocol::kFile: {
      return rights & (fio::Rights::kReadBytes | fio::Rights::kWriteBytes | fio::Rights::kExecute |
                       fio::Rights::kGetAttributes | fio::Rights::kUpdateAttributes);
    }
    case VnodeProtocol::kNode: {
      // Node connections only support GET_ATTRIBUTES.
      return rights & fio::Rights::kGetAttributes;
    }
    default: {
      // Remove all rights from unknown or unsupported node types.
      return {};
    }
  }
}

namespace {

constexpr fio::NodeProtocolKinds FlagsToProtocols(fio::Flags flags) {
  fio::NodeProtocolKinds protocols;
  if (flags & fio::Flags::kProtocolDirectory) {
    protocols |= fio::NodeProtocolKinds::kDirectory;
  }
  if (flags & fio::Flags::kProtocolFile) {
    protocols |= fio::NodeProtocolKinds::kFile;
  }
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (flags & fio::Flags::kProtocolSymlink) {
    protocols |= fio::NodeProtocolKinds::kSymlink;
  }
#endif
  if (flags & fio::Flags::kProtocolService) {
    protocols |= fio::NodeProtocolKinds::kConnector;
  }
  return protocols;
}

}  // namespace

zx::result<VnodeProtocol> NegotiateProtocol(fio::Flags flags, fio::NodeProtocolKinds supported) {
  using fio::NodeProtocolKinds;

  const NodeProtocolKinds requested = FlagsToProtocols(flags);

  if (flags & fio::Flags::kProtocolNode) {
    if (!requested || (requested & supported)) {
      return zx::ok(VnodeProtocol::kNode);
    }
  } else {
    // Remove protocols that were not requested from the set of supported protocols. If no protocols
    // were requested, any protocol is acceptable.
    if (requested) {
      supported = supported & requested;
    }
    // Attempt to negotiate a protocol for the connection based on the following order. The
    // fuchsia.io protocol does not enforce a particular order for resolution, and when callers
    // specify multiple protocols, they must be prepared to accept any that were set in the request.
    if (supported & NodeProtocolKinds::kConnector) {
      return zx::ok(VnodeProtocol::kService);
    }
    if (supported & NodeProtocolKinds::kDirectory) {
      return zx::ok(VnodeProtocol::kDirectory);
    }
    if (supported & NodeProtocolKinds::kFile) {
      return zx::ok(VnodeProtocol::kFile);
    }
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    if (supported & NodeProtocolKinds::kSymlink) {
      return zx::ok(VnodeProtocol::kSymlink);
    }
#endif
  }
  // If we failed to resolve a protocol, we determine what error to return from a combination of the
  // type of node and the protocols which were requested.
  if ((requested & NodeProtocolKinds::kDirectory) && !(supported & NodeProtocolKinds::kDirectory)) {
    return zx::error(ZX_ERR_NOT_DIR);
  }
  if ((requested & NodeProtocolKinds::kFile) && !(supported & NodeProtocolKinds::kFile)) {
    return zx::error(ZX_ERR_NOT_FILE);
  }
  return zx::error(ZX_ERR_WRONG_TYPE);
}

zx::result<VnodeProtocol> NegotiateProtocol(fio::NodeProtocolKinds supported,
                                            fio::NodeProtocolKinds requested) {
  using fio::NodeProtocolKinds;
  // Remove protocols that were not requested from the set of supported protocols.
  supported = supported & requested;
  // Attempt to negotiate a protocol for the connection based on the following order. The fuchsia.io
  // protocol does not enforce a particular order for resolution, and when callers specify multiple
  // protocols, they must be prepared to accept any that were set in the request.
  if (supported & NodeProtocolKinds::kConnector) {
    return zx::ok(VnodeProtocol::kService);
  }
  if (supported & NodeProtocolKinds::kDirectory) {
    return zx::ok(VnodeProtocol::kDirectory);
  }
  if (supported & NodeProtocolKinds::kFile) {
    return zx::ok(VnodeProtocol::kFile);
  }
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (supported & NodeProtocolKinds::kSymlink) {
    return zx::ok(VnodeProtocol::kSymlink);
  }
#endif
  // If we failed to resolve a protocol, we determine what error to return from a combination of the
  // type of node and the protocols which were requested.
  if ((requested & NodeProtocolKinds::kDirectory) && !(supported & NodeProtocolKinds::kDirectory)) {
    return zx::error(ZX_ERR_NOT_DIR);
  }
  if ((requested & NodeProtocolKinds::kFile) && !(supported & NodeProtocolKinds::kFile)) {
    return zx::error(ZX_ERR_NOT_FILE);
  }
  return zx::error(ZX_ERR_WRONG_TYPE);
}

namespace {
// Helper function to reduce verbosity in |NodeAttributes:Build| impl below. Returns an external
// (non-owning) |fidl::ObjectView| to |obj|.
template <typename T>
fidl::ObjectView<T> ExternalView(T& obj) {
  return fidl::ObjectView<T>::FromExternal(&obj);
}
}  // namespace

zx::result<fio::wire::NodeAttributes2> NodeAttributeBuilder::Build(const Vnode& vnode,
                                                                   fio::NodeAttributesQuery query) {
  zx::result vnode_attrs = vnode.GetAttributes();
  if (vnode_attrs.is_error()) {
    return vnode_attrs.take_error();
  }
  attributes = *vnode_attrs;
  // Immutable attributes:
  auto immutable_builder = ImmutableAttrs::ExternalBuilder(
      fidl::ObjectView<fidl::WireTableFrame<ImmutableAttrs>>::FromExternal(&immutable_frame));
  if (query & fio::NodeAttributesQuery::kProtocols) {
    protocols = vnode.GetProtocols();
    immutable_builder.protocols(ExternalView(protocols));
  }
  if (query & fio::NodeAttributesQuery::kAbilities) {
    abilities = vnode.GetAbilities();
    immutable_builder.abilities(ExternalView(abilities));
  }
  if (query & fio::NodeAttributesQuery::kContentSize && attributes.content_size) {
    immutable_builder.content_size(ExternalView(*attributes.content_size));
  }
  if (query & fio::NodeAttributesQuery::kStorageSize && attributes.storage_size) {
    immutable_builder.storage_size(ExternalView(*attributes.storage_size));
  }
  if (query & fio::NodeAttributesQuery::kLinkCount && attributes.link_count) {
    immutable_builder.link_count(ExternalView(*attributes.link_count));
  }
  if (query & fio::NodeAttributesQuery::kId && attributes.id) {
    immutable_builder.id(ExternalView(*attributes.id));
  }
  // Mutable attributes:
  auto mutable_builder = MutableAttrs::ExternalBuilder(
      fidl::ObjectView<fidl::WireTableFrame<MutableAttrs>>::FromExternal(&mutable_frame));
  if (query & fio::NodeAttributesQuery::kCreationTime && attributes.creation_time) {
    mutable_builder.creation_time(ExternalView(*attributes.creation_time));
  }
  if (query & fio::NodeAttributesQuery::kModificationTime && attributes.modification_time) {
    mutable_builder.modification_time(ExternalView(*attributes.modification_time));
  }
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(18)
  if (query & fio::NodeAttributesQuery::kMode && attributes.mode) {
    mutable_builder.mode(*attributes.mode);
  }
  if (query & fio::NodeAttributesQuery::kUid && attributes.uid) {
    mutable_builder.uid(*attributes.uid);
  }
  if (query & fio::NodeAttributesQuery::kGid && attributes.gid) {
    mutable_builder.gid(*attributes.gid);
  }
  if (query & fio::NodeAttributesQuery::kRdev && attributes.rdev) {
    mutable_builder.rdev(ExternalView(*attributes.rdev));
  }
#endif

  // Build the wire table, which is now valid as long as this object remains in scope.
  return zx::ok(NodeAttributes2{
      .mutable_attributes = mutable_builder.Build(),
      .immutable_attributes = immutable_builder.Build(),
  });
}

uint32_t GetPosixMode(fio::NodeProtocolKinds protocols, fio::Abilities abilities) {
  uint32_t mode = 0;
  if (protocols & fio::NodeProtocolKinds::kDirectory) {
    mode |= V_TYPE_DIR;
    if (abilities & fio::Abilities::kEnumerate) {
      mode |= V_IRUSR;
    }
    if (abilities & fio::Abilities::kModifyDirectory) {
      mode |= V_IWUSR;
    }
    if (abilities & fio::Abilities::kTraverse) {
      mode |= V_IXUSR;
    }
  } else {
    mode |= V_TYPE_FILE;
    if (abilities & fio::Abilities::kReadBytes) {
      mode |= V_IRUSR;
    }
    if (abilities & fio::Abilities::kWriteBytes) {
      mode |= V_IWUSR;
    }
    if (abilities & fio::Abilities::kExecute) {
      mode |= V_IXUSR;
    }
  }
  return mode;
}

}  // namespace internal

}  // namespace fs
