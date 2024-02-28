// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_VFS_TYPES_H_
#define SRC_STORAGE_LIB_VFS_CPP_VFS_TYPES_H_

#include <fidl/fuchsia.io/cpp/natural_types.h>
#include <fidl/fuchsia.io/cpp/wire_types.h>
#include <lib/fdio/vfs.h>
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
#include <utility>
#include <variant>

#include <fbl/bits.h>

// The filesystem server exposes various FIDL protocols on top of the Vnode abstractions. This
// header defines some helper types composed with the fuchsia.io protocol types to better express
// API requirements. These type names should start with "Vnode" to reduce confusion with their FIDL
// counterparts.
namespace fs {

union Rights {
  uint32_t raw_value = 0;
  fbl::BitFieldMember<uint32_t, 0, 1> read;
  fbl::BitFieldMember<uint32_t, 1, 1> write;
  fbl::BitFieldMember<uint32_t, 3, 1> execute;

  explicit constexpr Rights(uint32_t initial = 0) : raw_value(initial) {}

  // True if any right is present.
  bool any() const { return raw_value != 0; }

  Rights& operator|=(Rights other) {
    raw_value |= other.raw_value;
    return *this;
  }

  constexpr Rights& operator=(const Rights& other) {
    raw_value = other.raw_value;
    return *this;
  }

  constexpr Rights(const Rights& other) = default;

  // Returns true if the rights does not exceed those in |other|.
  bool StricterOrSameAs(Rights other) const { return (raw_value & ~(other.raw_value)) == 0; }

  // Convenience factory functions for commonly used option combinations.
  constexpr static Rights ReadOnly() {
    Rights rights{};
    rights.read = true;
    return rights;
  }

  constexpr static Rights WriteOnly() {
    Rights rights{};
    rights.write = true;
    return rights;
  }

  constexpr static Rights ReadWrite() {
    Rights rights{};
    rights.read = true;
    rights.write = true;
    return rights;
  }

  constexpr static Rights ReadExec() {
    Rights rights{};
    rights.read = true;
    rights.execute = true;
    return rights;
  }

  constexpr static Rights WriteExec() {
    Rights rights{};
    rights.write = true;
    rights.execute = true;
    return rights;
  }

  constexpr static Rights All() {
    Rights rights{};
    rights.read = true;
    rights.write = true;
    rights.execute = true;
    return rights;
  }
};

constexpr Rights operator&(Rights lhs, Rights rhs) { return Rights(lhs.raw_value & rhs.raw_value); }

// Identifies a single type of node protocol where required for protocol negotiation/resolution.
enum class VnodeProtocol : uint8_t {
  kDirectory,
  kFile,
  kNode,
  kService,
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  kSymlink,
#endif
};

// Options specified during opening and cloning.
struct VnodeConnectionOptions {
  union Flags {
    uint32_t raw_value = 0;
    fbl::BitFieldMember<uint32_t, 0, 1> create;
    fbl::BitFieldMember<uint32_t, 1, 1> fail_if_exists;
    fbl::BitFieldMember<uint32_t, 2, 1> truncate;
    fbl::BitFieldMember<uint32_t, 3, 1> directory;
    fbl::BitFieldMember<uint32_t, 4, 1> not_directory;
    fbl::BitFieldMember<uint32_t, 5, 1> append;
    fbl::BitFieldMember<uint32_t, 6, 1> node_reference;
    fbl::BitFieldMember<uint32_t, 7, 1> describe;
    fbl::BitFieldMember<uint32_t, 8, 1> posix_write;
    fbl::BitFieldMember<uint32_t, 9, 1> posix_execute;
    fbl::BitFieldMember<uint32_t, 10, 1> clone_same_rights;

    constexpr Flags() = default;

    constexpr Flags& operator=(const Flags& other) {
      raw_value = other.raw_value;
      return *this;
    }

    constexpr Flags(const Flags& other) = default;

  } flags = {};

  Rights rights{};

  constexpr VnodeConnectionOptions set_directory() {
    flags.directory = true;
    return *this;
  }

  constexpr VnodeConnectionOptions set_not_directory() {
    flags.not_directory = true;
    return *this;
  }

  constexpr VnodeConnectionOptions set_node_reference() {
    flags.node_reference = true;
    return *this;
  }

  constexpr VnodeConnectionOptions set_truncate() {
    flags.truncate = true;
    return *this;
  }

  constexpr VnodeConnectionOptions set_create() {
    flags.create = true;
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
    if (flags.directory) {
      return fuchsia_io::NodeProtocolKinds::kDirectory;
    }
    if (flags.not_directory) {
      return kSupportedIo1Protocols ^ fuchsia_io::NodeProtocolKinds::kDirectory;
    }
    return kSupportedIo1Protocols;
  }

#ifdef __Fuchsia__
  // Converts from fuchsia.io v1 flags to |VnodeConnectionOptions|.
  static VnodeConnectionOptions FromIoV1Flags(fuchsia_io::OpenFlags fidl_flags);

  // Converts from |VnodeConnectionOptions| to fuchsia.io flags.
  fuchsia_io::OpenFlags ToIoV1Flags() const;

  // Some flags (e.g. POSIX) only affect the interpretation of rights at the time of Open/Clone, and
  // should have no effects thereafter. Hence we filter them here.
  // TODO(https://fxbug.dev/42108521): Some of these flag groups should be defined in fuchsia.io and
  // use that as the source of truth.
  static VnodeConnectionOptions FilterForNewConnection(VnodeConnectionOptions options);
#endif  // __Fuchsia__
};

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

#ifdef __Fuchsia__
  // Converts from |VnodeAttributes| to fuchsia.io v1 |NodeAttributes|.
  fuchsia_io::wire::NodeAttributes ToIoV1NodeAttributes() const;
#endif  // __Fuchsia__
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

#ifdef __Fuchsia__

// Represents an fuchsia.io/OnRepresentation response. Used instead of |fuchsia_io::Representation|
// directly to prevent extra heap allocations when making wire calls.
using VnodeRepresentation =
#if __Fuchsia_API_level__ < 18
    std::variant<fuchsia_io::ConnectorInfo, fuchsia_io::DirectoryInfo, fuchsia_io::FileInfo>;
#else
    std::variant<fuchsia_io::ConnectorInfo, fuchsia_io::DirectoryInfo, fuchsia_io::FileInfo,
                 fuchsia_io::SymlinkInfo>;
#endif

// Consume an io2 |representation|, and convert to equivalent |fuchsia_io::wire::NodeInfoDeprecated|
// to be handled with |handler|. A callback is used to avoid lifetime issues, which would require
// use of an arena if the value were to be returned.
void HandleAsNodeInfoDeprecated(VnodeRepresentation representation,
                                fit::callback<void(fuchsia_io::wire::NodeInfoDeprecated)> handler);

#endif  // __Fuchsia__

}  // namespace fs

#endif  // SRC_STORAGE_LIB_VFS_CPP_VFS_TYPES_H_
