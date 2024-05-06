// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_VFS_TYPES_H_
#define SRC_STORAGE_LIB_VFS_CPP_VFS_TYPES_H_

#include <fidl/fuchsia.io/cpp/natural_types.h>
#include <fidl/fuchsia.io/cpp/wire_types.h>
#include <lib/fdio/vfs.h>
#include <lib/zx/result.h>
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

namespace Rights {

// Alias for commonly used read-only directory rights.
constexpr fuchsia_io::Rights ReadOnly() { return fuchsia_io::kRStarDir; }

// Alias for commonly used write-only directory rights.
constexpr fuchsia_io::Rights WriteOnly() {
  // TODO(https://fxbug.dev/42157659): Restrict GET_ATTRIBUTES.
  return fuchsia_io::Rights::kGetAttributes | fuchsia_io::kWStarDir;
}

// Alias for commonly used read-write directory rights.
constexpr fuchsia_io::Rights ReadWrite() { return fuchsia_io::kRwStarDir; }

// Alias for commonly used read-execute directory rights.
constexpr fuchsia_io::Rights ReadExec() { return fuchsia_io::kRxStarDir; }

// Alias for all possible rights.
constexpr fuchsia_io::Rights All() { return fuchsia_io::Rights::kMask; }

}  // namespace Rights

constexpr fuchsia_io::OpenFlags kAllIo1Rights = fuchsia_io::OpenFlags::kRightReadable |
                                                fuchsia_io::OpenFlags::kRightWritable |
                                                fuchsia_io::OpenFlags::kRightExecutable;

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
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  kSymlink = uint64_t{fuchsia_io::NodeProtocolKinds::kSymlink},
#endif
};

inline zx::result<VnodeProtocol> NegotiateProtocol(fuchsia_io::NodeProtocolKinds protocols) {
  if (protocols & fuchsia_io::NodeProtocolKinds::kConnector) {
    return zx::ok(VnodeProtocol::kService);
  }
  if (protocols & fuchsia_io::NodeProtocolKinds::kDirectory) {
    return zx::ok(VnodeProtocol::kDirectory);
  }
  if (protocols & fuchsia_io::NodeProtocolKinds::kFile) {
    return zx::ok(VnodeProtocol::kFile);
  }
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  if (protocols & fuchsia_io::NodeProtocolKinds::kSymlink) {
    return zx::ok(VnodeProtocol::kSymlink);
  }
#endif
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

// Options specified during opening and cloning.
struct VnodeConnectionOptions {
  fuchsia_io::OpenFlags flags;
  fuchsia_io::Rights rights;

  // TODO(https://fxbug.dev/324112857): Remove the following setters, as some aren't compatible with
  // io2 directly (e.g. not_directory has no equivalent, and set_truncate only applies if the file
  // protocol was selected). These setters are only used in tests anyways - same with the factory
  // functions below.

  constexpr VnodeConnectionOptions set_directory() {
    flags |= fuchsia_io::OpenFlags::kDirectory;
    return *this;
  }

  constexpr VnodeConnectionOptions set_not_directory() {
    flags |= fuchsia_io::OpenFlags::kNotDirectory;
    return *this;
  }

  constexpr VnodeConnectionOptions set_node_reference() {
    flags |= fuchsia_io::OpenFlags::kNodeReference;
    return *this;
  }

  constexpr VnodeConnectionOptions set_truncate() {
    flags |= fuchsia_io::OpenFlags::kTruncate;
    return *this;
  }

  // Convenience factory functions for commonly used option combinations.

  constexpr static VnodeConnectionOptions ReadOnly() {
    VnodeConnectionOptions options;
    options.rights = Rights::ReadOnly();
    return options;
  }

  constexpr static VnodeConnectionOptions WriteOnly() {
    VnodeConnectionOptions options;
    options.rights = Rights::WriteOnly();
    return options;
  }

  constexpr static VnodeConnectionOptions ReadWrite() {
    VnodeConnectionOptions options;
    options.rights = Rights::ReadWrite();
    return options;
  }

  constexpr static VnodeConnectionOptions ReadExec() {
    VnodeConnectionOptions options;
    options.rights = Rights::ReadExec();
    return options;
  }

  // Translates the io1 flags passed by the client into an equivalent set of io2 protocols.
  constexpr fuchsia_io::NodeProtocolKinds protocols() const {
    constexpr fuchsia_io::NodeProtocolKinds kSupportedIo1Protocols =
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
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

  // Converts from fuchsia.io/Directory.Clone flags to |VnodeConnectionOptions|. Note that in io1,
  // certain operations were unprivileged so they may be implicitly added to the resulting `rights`.
  static zx::result<VnodeConnectionOptions> FromCloneFlags(fuchsia_io::OpenFlags flags);

  // Converts from |VnodeConnectionOptions| to fuchsia.io flags.
  fuchsia_io::OpenFlags ToIoV1Flags() const;
};

fuchsia_io::OpenFlags RightsToOpenFlags(fuchsia_io::Rights rights);

// Objective information about a filesystem node, used to implement |Vnode::GetAttributes|.
struct VnodeAttributes {
  uint32_t mode = {};
  uint64_t inode = {};
  uint64_t content_size = {};
  uint64_t storage_size = {};
  uint64_t link_count = {};
  uint64_t creation_time = {};
  uint64_t modification_time = {};

  bool operator==(const VnodeAttributes& other) const {
    return mode == other.mode && inode == other.inode && content_size == other.content_size &&
           storage_size == other.storage_size && link_count == other.link_count &&
           creation_time == other.creation_time && modification_time == other.modification_time;
  }

  // Converts from |VnodeAttributes| to fuchsia.io v1 |NodeAttributes|.
  fuchsia_io::wire::NodeAttributes ToIoV1NodeAttributes() const;
};

// A request to update pieces of the |VnodeAttributes|. The fuchsia.io protocol only allows mutating
// the creation time and modification time. When a field is present, it indicates that the
// corresponding field should be updated.
class VnodeAttributesUpdate {
 public:
  VnodeAttributesUpdate& set_creation_time(std::optional<uint64_t> v) {
    creation_time_ = v;
    return *this;
  }

  VnodeAttributesUpdate& set_modification_time(std::optional<uint64_t> v) {
    modification_time_ = v;
    return *this;
  }

  bool any() const { return creation_time_.has_value() || modification_time_.has_value(); }

  bool has_creation_time() const { return creation_time_.has_value(); }

  // Moves out the creation time. Requires |creation_time_| to be present. After this method
  // returns, |creation_time_| is absent.
  uint64_t take_creation_time() {
    uint64_t v = creation_time_.value();
    creation_time_ = std::nullopt;
    return v;
  }

  bool has_modification_time() const { return modification_time_.has_value(); }

  // Moves out the modification time. Requires |modification_time_| to be present. After this method
  // returns, |modification_time_| is absent.
  uint64_t take_modification_time() {
    uint64_t v = modification_time_.value();
    modification_time_ = std::nullopt;
    return v;
  }

 private:
  std::optional<uint64_t> creation_time_ = {};
  std::optional<uint64_t> modification_time_ = {};
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

#if !defined(__Fuchsia__) || __Fuchsia_API_level__ >= 19
constexpr CreationMode CreationModeFromFidl(fuchsia_io::CreationMode mode) {
  switch (mode) {
    case fuchsia_io::CreationMode::kNever:
    case fuchsia_io::CreationMode::kNeverDeprecated:
      return CreationMode::kNever;
    case fuchsia_io::CreationMode::kAllowExisting:
      return CreationMode::kAllowExisting;
    case fuchsia_io::CreationMode::kAlways:
      return CreationMode::kAlways;
  }
}

constexpr fuchsia_io::CreationMode CreationModeToFidl(CreationMode mode) {
  switch (mode) {
    case CreationMode::kNever:
      return fuchsia_io::CreationMode::kNever;
    case CreationMode::kAllowExisting:
      return fuchsia_io::CreationMode::kAllowExisting;
    case CreationMode::kAlways:
      return fuchsia_io::CreationMode::kAlways;
  }
}
#else
constexpr CreationMode CreationModeFromFidl(fuchsia_io::OpenMode mode) {
  switch (mode) {
    case fuchsia_io::OpenMode::kOpenExisting:
      return CreationMode::kNever;
    case fuchsia_io::OpenMode::kMaybeCreate:
      return CreationMode::kAllowExisting;
    case fuchsia_io::OpenMode::kAlwaysCreate:
      return CreationMode::kAlways;
  }
}

constexpr fuchsia_io::OpenMode CreationModeToFidl(CreationMode mode) {
  switch (mode) {
    case CreationMode::kNever:
      return fuchsia_io::OpenMode::kOpenExisting;
    case CreationMode::kAllowExisting:
      return fuchsia_io::OpenMode::kMaybeCreate;
    case CreationMode::kAlways:
      return fuchsia_io::OpenMode::kAlwaysCreate;
  }
}
#endif

constexpr CreationMode CreationModeFromFidl(fuchsia_io::OpenFlags flags) {
  if (flags & fuchsia_io::OpenFlags::kCreateIfAbsent) {
    return CreationMode::kAlways;
  }
  if (flags & fuchsia_io::OpenFlags::kCreate) {
    return CreationMode::kAllowExisting;
  }
  return CreationMode::kNever;
}

}  // namespace internal

}  // namespace fs

#endif  // SRC_STORAGE_LIB_VFS_CPP_VFS_TYPES_H_
