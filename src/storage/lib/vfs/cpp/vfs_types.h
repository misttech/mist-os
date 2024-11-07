// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_VFS_TYPES_H_
#define SRC_STORAGE_LIB_VFS_CPP_VFS_TYPES_H_

#include <fidl/fuchsia.io/cpp/natural_types.h>
#include <fidl/fuchsia.io/cpp/wire_types.h>
#include <lib/fdio/vfs.h>
#include <lib/zx/result.h>
#include <zircon/availability.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#ifdef __Fuchsia__
#include <lib/zx/channel.h>
#include <lib/zx/event.h>
#include <lib/zx/eventpair.h>
#include <lib/zx/handle.h>
#include <lib/zx/socket.h>
#include <lib/zx/stream.h>
#include <lib/zx/vmo.h>
#endif

#include <cstdint>
#include <cstring>
#include <optional>

#include <fbl/bits.h>

// The filesystem server exposes various FIDL protocols on top of the Vnode abstractions. This
// header defines some helper types composed with the fuchsia.io protocol types to better express
// API requirements. These type names should start with "Vnode" to reduce confusion with their FIDL
// counterparts.
namespace fs {

class Vnode;

// All io1 OpenFlags that correspond to connection rights.
constexpr fuchsia_io::OpenFlags kAllIo1Rights = fuchsia_io::OpenFlags::kRightReadable |
                                                fuchsia_io::OpenFlags::kRightWritable |
                                                fuchsia_io::OpenFlags::kRightExecutable;

// All io2 Rights that allow a connection to modify the filesystem.
constexpr fuchsia_io::Rights kAllMutableIo2Rights = fuchsia_io::Rights::kWriteBytes |
                                                    fuchsia_io::Rights::kModifyDirectory |
                                                    fuchsia_io::Rights::kUpdateAttributes;

// Specifies the type of object when creating new nodes.
enum class CreationType : uint8_t {
  kFile = 0,
  kDirectory = 1,
  // Max value used for fuzzing.
  kMaxValue = kDirectory,
};

// Identifies a single type of node protocol where required for protocol negotiation/resolution.
enum class VnodeProtocol : uint8_t {
  kNode = 0,  // All Vnodes support fuchsia.io/Node, so it does not have an explicit representation.
  kService = uint64_t{fuchsia_io::NodeProtocolKinds::kConnector},
  kDirectory = uint64_t{fuchsia_io::NodeProtocolKinds::kDirectory},
  kFile = uint64_t{fuchsia_io::NodeProtocolKinds::kFile},
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  kSymlink = uint64_t{fuchsia_io::NodeProtocolKinds::kSymlink},
#endif
};

// Options specified during opening and cloning.
struct VnodeConnectionOptions {
  fuchsia_io::OpenFlags flags;
  fuchsia_io::Rights rights;

  // Translates the io1 flags passed by the client into an equivalent set of io2 protocols.
  constexpr fuchsia_io::NodeProtocolKinds protocols() const {
    constexpr fuchsia_io::NodeProtocolKinds kSupportedIo1Protocols =
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
        // Symlinks are not supported via io1.
        fuchsia_io::NodeProtocolKinds::kMask ^ fuchsia_io::NodeProtocolKinds::kSymlink;
#else
        fuchsia_io::NodeProtocolKinds::kMask;
#endif
    if (flags & fuchsia_io::OpenFlags::kDirectory) {
      return fuchsia_io::NodeProtocolKinds::kDirectory;
    }
    if (flags & fuchsia_io::OpenFlags::kNotDirectory) {
      return kSupportedIo1Protocols ^ fuchsia_io::NodeProtocolKinds::kDirectory;
    }
    return kSupportedIo1Protocols;
  }

  // Converts from fuchsia.io/Directory.Open1 flags to |VnodeConnectionOptions|. Note that in io1,
  // certain operations were unprivileged so they may be implicitly added to the resulting `rights`.
  static zx::result<VnodeConnectionOptions> FromOpen1Flags(fuchsia_io::OpenFlags flags);

  // Converts from fuchsia.io/Directory.Clone flags to |VnodeConnectionOptions|.
  static zx::result<VnodeConnectionOptions> FromCloneFlags(fuchsia_io::OpenFlags flags,
                                                           VnodeProtocol protocol);

  // Converts from |VnodeConnectionOptions| to fuchsia.io flags.
  fuchsia_io::OpenFlags ToIoV1Flags() const;
};

fuchsia_io::OpenFlags RightsToOpenFlags(fuchsia_io::Rights rights);

using VnodeAttributesQuery = fuchsia_io::NodeAttributesQuery;

// Objective information about a filesystem node, used to implement |Vnode::GetAttributes|.
// Filesystems should only report those attributes which it has support for.
//
// Note that only attributes for which existing filesystems support are currently implemented.
// Additional attributes can be supported by adding them to this struct, and updating the
// |NodeAttributeBuilder::Build()| function accordingly.
struct VnodeAttributes {
  std::optional<uint64_t> id;
  std::optional<uint64_t> content_size;
  std::optional<uint64_t> storage_size;
  std::optional<uint64_t> link_count;

  std::optional<uint64_t> creation_time;
  std::optional<uint64_t> modification_time;

  // POSIX Compatibility Attributes
  std::optional<uint32_t> mode;
  std::optional<uint32_t> uid;
  std::optional<uint32_t> gid;
  std::optional<uint64_t> rdev;

  // Compare two |VnodeAttributes| instances for equality.
  bool operator==(const VnodeAttributes& other) const {
    return id == other.id && content_size == other.content_size &&
           storage_size == other.storage_size && link_count == other.link_count &&
           creation_time == other.creation_time && modification_time == other.modification_time &&
           mode == other.mode && uid == other.uid && gid == other.gid && rdev == other.rdev;
  }

  // Converts from |VnodeAttributes| to fuchsia.io v1 |NodeAttributes|.
  fuchsia_io::wire::NodeAttributes ToIoV1NodeAttributes(const fs::Vnode& vnode) const;
};

// A request to update pieces of the |VnodeAttributes| via |Vnode::SetAttributes|. Filesystems may
// only support a sub-set of all possible attributes.
//
// Note that only attributes for which existing filesystems support are currently implemented.
// Additional attributes can be supported by adding them to this struct, and updating the
// |FromIo1()| and |FromIo2()| functions accordingly.
struct VnodeAttributesUpdate {
  std::optional<uint64_t> creation_time;
  std::optional<uint64_t> modification_time;
  std::optional<uint64_t> access_time;

  // POSIX Compatibility Attributes
  std::optional<uint32_t> mode;
  std::optional<uint32_t> uid;
  std::optional<uint32_t> gid;
  std::optional<uint64_t> rdev;

  // Return a set of flags representing those attributes which we want to update.
  constexpr VnodeAttributesQuery Query() const {
    VnodeAttributesQuery query;
    if (creation_time) {
      query |= VnodeAttributesQuery::kCreationTime;
    }
    if (modification_time) {
      query |= VnodeAttributesQuery::kModificationTime;
    }
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(18)
    if (mode) {
      query |= VnodeAttributesQuery::kMode;
    }
    if (uid) {
      query |= VnodeAttributesQuery::kUid;
    }
    if (gid) {
      query |= VnodeAttributesQuery::kGid;
    }
    if (rdev) {
      query |= VnodeAttributesQuery::kRdev;
    }
    if (access_time) {
      query |= VnodeAttributesQuery::kAccessTime;
    }
#endif
    return query;
  }

  static constexpr VnodeAttributesUpdate FromIo1(const fuchsia_io::wire::NodeAttributes& attrs,
                                                 fuchsia_io::NodeAttributeFlags flags) {
    VnodeAttributesUpdate attr_update;
    if (flags & fuchsia_io::NodeAttributeFlags::kCreationTime) {
      attr_update.creation_time = attrs.creation_time;
    }
    if (flags & fuchsia_io::NodeAttributeFlags::kModificationTime) {
      attr_update.modification_time = attrs.modification_time;
    }
    return attr_update;
  }

  static constexpr VnodeAttributesUpdate FromIo2(
      const fuchsia_io::wire::MutableNodeAttributes& attrs) {
    VnodeAttributesUpdate attr_update;
    if (attrs.has_creation_time()) {
      attr_update.creation_time = attrs.creation_time();
    }
    if (attrs.has_modification_time()) {
      attr_update.modification_time = attrs.modification_time();
    }
#if !defined(__Fuchsia__) || FUCHSIA_API_LEVEL_AT_LEAST(18)
    if (attrs.has_mode()) {
      attr_update.mode = attrs.mode();
    }
    if (attrs.has_uid()) {
      attr_update.uid = attrs.uid();
    }
    if (attrs.has_gid()) {
      attr_update.gid = attrs.gid();
    }
    if (attrs.has_rdev()) {
      attr_update.rdev = attrs.rdev();
    }
    if (attrs.has_access_time()) {
      attr_update.access_time = attrs.access_time();
    }
#endif
    return attr_update;
  }
};

// Indicates if and when a new object should be created when opening a node.
enum class CreationMode : uint8_t {
  // Never create an object. Will return `ZX_ERR_NOT_FOUND` if there is no existing object.
  kNever,
  // Create a new object if one doesn't already exist, otherwise open the existing object.
  kAllowExisting,
  // Always create an object. Will return `ZX_ERR_ALREADY_EXISTS` if one already exists.
  kAlways,
};

namespace internal {

// Downscope |rights| to only include those operations which |protocol| supports, or those which are
// applicable to child nodes. This follows the principle of least privilege.
fuchsia_io::Rights DownscopeRights(fuchsia_io::Rights rights, VnodeProtocol protocol);

// Determines the protocol to use for serving a connection, based on the |supported| protocols for
// a node, and those which were |requested|.
//
// Note that this function is not part of the |Vnode| interface. The fuchsia.io protocol does not
// define a specific order of protocol resolution when |requested| is ambiguous, but we define a
// strict mapping here to enforce consistency across the Fuchsia VFS libraries.
zx::result<VnodeProtocol> NegotiateProtocol(fuchsia_io::NodeProtocolKinds supported,
                                            fuchsia_io::NodeProtocolKinds requested);

// Determines the protocol to use for serving a connection, based on the |supported| protocols for
// a node, and those which were requested in |flags|.
zx::result<VnodeProtocol> NegotiateProtocol(fuchsia_io::Flags flags,
                                            fuchsia_io::NodeProtocolKinds supported);

// Synthesizes a set of POSIX mode bits using a node's supported protocols and abilities.
// This implementation mirrors that of |zxio_get_posix_mode|.
//
// Unlike the ZXIO implementation, this function is *only* used for synthesizing the mode bits
// reported by the io1 GetAttrs method. Callers should use the io2 GetAttributes method to get
// an accurate representation of the mode bits.
uint32_t GetPosixMode(fuchsia_io::NodeProtocolKinds protocols, fuchsia_io::Abilities abilities);

constexpr fuchsia_io::NodeProtocolKinds GetProtocols(fuchsia_io::Flags flags) {
  using fuchsia_io::Flags;
  using fuchsia_io::NodeProtocolKinds;
  // If the caller didn't specify a protocol, allow any.
  if (!(flags & fuchsia_io::kMaskKnownProtocols)) {
    return NodeProtocolKinds::kMask;
  }
  if (flags & Flags::kProtocolService) {
    return NodeProtocolKinds::kConnector;
  }
  NodeProtocolKinds protocols;
  if (flags & Flags::kProtocolDirectory) {
    protocols |= NodeProtocolKinds::kDirectory;
  }
  if (flags & Flags::kProtocolFile) {
    protocols |= NodeProtocolKinds::kFile;
  }
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (flags & Flags::kProtocolSymlink) {
    protocols |= NodeProtocolKinds::kSymlink;
  }
#endif
  return protocols;
}

constexpr fuchsia_io::Rights FlagsToRights(fuchsia_io::Flags flags) {
  using fuchsia_io::Flags;
  using fuchsia_io::Rights;
  return static_cast<Rights>(static_cast<uint64_t>(flags) & static_cast<uint64_t>(Rights::kMask));
}

constexpr fuchsia_io::Flags RightsToFlags(fuchsia_io::Rights rights) {
  using fuchsia_io::Flags;
  using fuchsia_io::Rights;
  return static_cast<Flags>(static_cast<uint64_t>(rights));
}

// Encapsulates the state of a node's wire attributes on the stack. Used by connections for sending
// an OnRepresentation event or responding to a fuchsia.io/Node.GetAttributes call.
class NodeAttributeBuilder {
 public:
  using NodeAttributes2 = fuchsia_io::wire::NodeAttributes2;
  using ImmutableAttrs = fuchsia_io::wire::ImmutableNodeAttributes;
  using MutableAttrs = fuchsia_io::wire::MutableNodeAttributes;

  // Build and return a wire object that uses this object as storage. This object **must** outlive
  // the returned wire table. The resulting table will include all attributes specified by |query|
  // that the vnode supports.
  zx::result<NodeAttributes2> Build(const Vnode& vnode, fuchsia_io::NodeAttributesQuery query);

 private:
  // Data referenced by the table frames below:
  VnodeAttributes attributes;
  fuchsia_io::Abilities abilities;
  fuchsia_io::NodeProtocolKinds protocols;
  // Table frames the final wire object will be built from:
  fidl::WireTableFrame<ImmutableAttrs> immutable_frame;
  fidl::WireTableFrame<MutableAttrs> mutable_frame;
};

constexpr CreationMode CreationModeFromFidl(fuchsia_io::OpenFlags flags) {
  if (flags & fuchsia_io::OpenFlags::kCreateIfAbsent) {
    return CreationMode::kAlways;
  }
  if (flags & fuchsia_io::OpenFlags::kCreate) {
    return CreationMode::kAllowExisting;
  }
  return CreationMode::kNever;
}

constexpr CreationMode CreationModeFromFidl(fuchsia_io::Flags flags) {
  if (flags & fuchsia_io::Flags::kFlagMustCreate) {
    return CreationMode::kAlways;
  }
  if (flags & fuchsia_io::Flags::kFlagMaybeCreate) {
    return CreationMode::kAllowExisting;
  }
  return CreationMode::kNever;
}

}  // namespace internal

}  // namespace fs

#endif  // SRC_STORAGE_LIB_VFS_CPP_VFS_TYPES_H_
