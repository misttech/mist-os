// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.pty/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/common_types.h>
#include <fidl/fuchsia.io/cpp/natural_types.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire_types.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <lib/fit/defer.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/channel.h>
#include <lib/zxio/null.h>
#include <lib/zxio/ops.h>
#include <lib/zxio/posix_mode.h>
#include <lib/zxio/types.h>
#include <sys/stat.h>
#include <zircon/availability.h>
#include <zircon/compiler.h>
#include <zircon/fidl.h>
#include <zircon/syscalls.h>

#include <cstdio>

#include "sdk/lib/zxio/private.h"
#include "sdk/lib/zxio/vector.h"

namespace fdevice = fuchsia_device;
namespace fio = fuchsia_io;

namespace {

class Directory;

// Implementation of |zxio_dirent_iterator_t| for |fuchsia.io| v1.
class DirentIteratorImpl {
 public:
  explicit DirentIteratorImpl(const zxio_t* io) : io_(reinterpret_cast<const Directory*>(io)) {
    static_assert(offsetof(DirentIteratorImpl, io_) == 0,
                  "zxio_dirent_iterator_t requires first field of implementation to be zxio_t");
  }

  zx_status_t Next(zxio_dirent_t* inout_entry) {
    if (remaining_dirents_.empty()) {
      const zx_status_t status = RemoteReadDirents();
      if (status != ZX_OK) {
        return status;
      }
      if (remaining_dirents_.empty()) {
        return ZX_ERR_NOT_FOUND;
      }
    }

    // The format of the packed dirent structure, taken from io.fidl.
    struct dirent {
      // Describes the inode of the entry.
      uint64_t ino;
      // Describes the length of the dirent name in bytes.
      uint8_t size;
      // Describes the type of the entry. Aligned with the
      // POSIX d_type values. Use `DIRENT_TYPE_*` constants.
      uint8_t type;
      // Unterminated name of entry.
      char name[0];
    } __PACKED;

    // Check if we can read the entry size.
    if (remaining_dirents_.size_bytes() < sizeof(dirent)) {
      // Should not happen
      return ZX_ERR_INTERNAL;
    }

    const dirent& packed_entry = *reinterpret_cast<const dirent*>(remaining_dirents_.data());
    const size_t packed_entry_size = sizeof(dirent) + packed_entry.size;

    // Check if we can read the whole entry.
    if (remaining_dirents_.size_bytes() < packed_entry_size) {
      // Should not happen
      return ZX_ERR_INTERNAL;
    }

    // Check that the name length is within bounds.
    if (packed_entry.size > fio::wire::kMaxFilename) {
      return ZX_ERR_INVALID_ARGS;
    }

    remaining_dirents_ = remaining_dirents_.subspan(packed_entry_size);

    ZXIO_DIRENT_SET(*inout_entry, protocols, DTypeToProtocols(packed_entry.type));
    ZXIO_DIRENT_SET(*inout_entry, id, packed_entry.ino);
    inout_entry->name_length = packed_entry.size;
    if (inout_entry->name != nullptr) {
      memcpy(inout_entry->name, packed_entry.name, packed_entry.size);
    }

    return ZX_OK;
  }

  zx_status_t Rewind() {
    const fidl::WireResult result = client()->Rewind();
    if (!result.ok()) {
      return result.status();
    }
    if (result->s != ZX_OK) {
      return result->s;
    }

    // Reset the state of the iterator, forcing an update the next time |Next| is called.
    remaining_dirents_ = {};
    return ZX_OK;
  }

 private:
  const fidl::WireSyncClient<fio::Directory>& client() const;

  zx_status_t RemoteReadDirents() {
    fidl::BufferSpan fidl_buffer(buffer_, sizeof(buffer_));
    const fidl::WireUnownedResult result = client().buffer(fidl_buffer)->ReadDirents(kBufferSize);
    if (!result.ok()) {
      return result.status();
    }
    const auto& response = result.value();
    if (const zx_status_t status = response.s; status != ZX_OK) {
      return status;
    }
    const fidl::VectorView dirents = response.dirents;
    if (dirents.count() > kBufferSize) {
      return ZX_ERR_IO;
    }

    remaining_dirents_ = dirents.get();
    return ZX_OK;
  }

  static zxio_node_protocols_t DTypeToProtocols(uint8_t type) {
    switch (type) {
      case DT_DIR:
        return ZXIO_NODE_PROTOCOL_DIRECTORY;
      case DT_REG:
        return ZXIO_NODE_PROTOCOL_FILE;
      case DT_LNK:
        return ZXIO_NODE_PROTOCOL_SYMLINK;
      case DT_BLK:
        // Not supported.
      case DT_CHR:
        // Not supported.
      case DT_FIFO:
        // Not supported.
      case DT_SOCK:
        // Not supported.
      default:
        return ZXIO_NODE_PROTOCOL_NONE;
    }
  }

  // The maximum buffer size that is supported by |fuchsia.io/Directory.ReadDirents|.
  static constexpr size_t kBufferSize = fio::wire::kMaxBuf;

  const Directory* const io_;

  // Issuing a FIDL call requires storage for both the request (16 bytes) and the largest possible
  // response message (8192 bytes of payload).
  // TODO(https://fxbug.dev/42166787): Once overlapping request and response is allowed, reduce
  // this allocation to a single channel message size.
  FIDL_ALIGNDECL uint8_t
      buffer_[fidl::SyncClientMethodBufferSizeInChannel<fio::Directory::ReadDirents>()];

  // Tracks and holds a view into |buffer_| of remaining unread dirents.
  cpp20::span<uint8_t> remaining_dirents_;
};

static_assert(sizeof(DirentIteratorImpl) <= sizeof(zxio_dirent_iterator_t),
              "DirentIteratorImpl should fit within a zxio_dirent_iterator_t");

zxio_node_protocols_t ToZxioNodeProtocols(uint32_t mode) {
  switch (mode & (S_IFMT | fio::wire::kModeTypeService)) {
    case S_IFDIR:
      return ZXIO_NODE_PROTOCOL_DIRECTORY;
    case S_IFREG:
      return ZXIO_NODE_PROTOCOL_FILE;
    case fio::wire::kModeTypeService:
      // fuchsia::io has mode type service which breaks stat.
      // TODO(https://fxbug.dev/42130287): return ZXIO_NODE_PROTOCOL_CONNECTOR instead.
      return ZXIO_NODE_PROTOCOL_FILE;
    case S_IFLNK:
      return ZXIO_NODE_PROTOCOL_SYMLINK;
    case S_IFBLK:
      // Block-oriented devices are not supported on Fuchsia.
    case S_IFCHR:
      // Character-oriented devices are not supported on Fuchsia.
    case S_IFIFO:
      // Named pipes are not supported on Fuchsia.
    case S_IFSOCK:
      // Named sockets are not supported on Fuchsia.
    default:
      // A reasonable fallback is to keep the protocols unchanged,
      // i.e. same as getting a protocol we do not understand.
      return ZXIO_NODE_PROTOCOL_NONE;
  }
}

uint32_t ToIo1ModeFileType(zxio_node_protocols_t protocols) {
  // The "file type" portion of mode only allow one bit, so we find
  // the best approximation given some set of |protocols|, tie-breaking
  // in the following precedence.
  if (protocols & ZXIO_NODE_PROTOCOL_DIRECTORY) {
    return S_IFDIR;
  }
  if (protocols & ZXIO_NODE_PROTOCOL_FILE) {
    return S_IFREG;
  }
  if (protocols & ZXIO_NODE_PROTOCOL_CONNECTOR) {
    // There is no good analogue for FIDL services in POSIX land...
    // Returning "regular file" as a fallback.
    return S_IFREG;
  }
  if (protocols & ZXIO_NODE_PROTOCOL_SYMLINK) {
    return S_IFLNK;
  }
  return 0;
}

class ToZxioAbilitiesForFile {
 public:
  zxio_abilities_t operator()(uint32_t mode) {
    zxio_abilities_t abilities = ZXIO_OPERATION_NONE;
    if (mode & S_IRUSR) {
      abilities |= ZXIO_OPERATION_READ_BYTES;
    }
    if (mode & S_IWUSR) {
      abilities |= ZXIO_OPERATION_WRITE_BYTES;
    }
    if (mode & S_IXUSR) {
      abilities |= ZXIO_OPERATION_EXECUTE;
    }
    // In addition, POSIX seems to allow changing file metadata
    // regardless of read/write permissions, as long as we are the
    // owner.
    abilities |= ZXIO_OPERATION_GET_ATTRIBUTES;
    abilities |= ZXIO_OPERATION_UPDATE_ATTRIBUTES;
    return abilities;
  }
};

class ToIo1ModePermissionsForFile {
 public:
  uint32_t operator()(zxio_abilities_t abilities) {
    // Permissions are not applicable on Fuchsia.
    // We could approximate them using the |abilities| of a node.
    uint32_t permission_bits = 0;
    if (abilities & ZXIO_OPERATION_READ_BYTES) {
      permission_bits |= S_IRUSR;
    }
    if (abilities & ZXIO_OPERATION_WRITE_BYTES) {
      permission_bits |= S_IWUSR;
    }
    if (abilities & ZXIO_OPERATION_EXECUTE) {
      permission_bits |= S_IXUSR;
    }
    return permission_bits;
  }
};

class ToZxioAbilitiesForDirectory {
 public:
  zxio_abilities_t operator()(uint32_t mode) {
    zxio_abilities_t abilities = ZXIO_OPERATION_NONE;
    if (mode & S_IRUSR) {
      abilities |= ZXIO_OPERATION_ENUMERATE;
    }
    if (mode & S_IWUSR) {
      abilities |= ZXIO_OPERATION_MODIFY_DIRECTORY;
    }
    if (mode & S_IXUSR) {
      abilities |= ZXIO_OPERATION_TRAVERSE;
    }
    // In addition, POSIX seems to allow changing file metadata
    // regardless of read/write permissions, as long as we are the
    // owner.
    abilities |= ZXIO_OPERATION_GET_ATTRIBUTES;
    abilities |= ZXIO_OPERATION_UPDATE_ATTRIBUTES;
    return abilities;
  }
};

class ToIo1ModePermissionsForDirectory {
 public:
  uint32_t operator()(zxio_abilities_t abilities) {
    // Permissions are not applicable on Fuchsia.
    // We could approximate them using the |abilities| of a node.
    uint32_t permission_bits = 0;
    if (abilities & ZXIO_OPERATION_ENUMERATE) {
      permission_bits |= S_IRUSR;
    }
    if (abilities & ZXIO_OPERATION_MODIFY_DIRECTORY) {
      permission_bits |= S_IWUSR;
    }
    if (abilities & ZXIO_OPERATION_TRAVERSE) {
      permission_bits |= S_IXUSR;
    }
    return permission_bits;
  }
};

template <typename ToZxioAbilities>
void ToZxioNodeAttributes(fio::wire::NodeAttributes attr, ToZxioAbilities to_zxio,
                          zxio_node_attributes_t* inout_zxio_attr) {
  if (inout_zxio_attr->has.protocols)
    ZXIO_NODE_ATTR_SET(*inout_zxio_attr, protocols, ToZxioNodeProtocols(attr.mode));
  if (inout_zxio_attr->has.abilities)
    ZXIO_NODE_ATTR_SET(*inout_zxio_attr, abilities, to_zxio(attr.mode));
  if (inout_zxio_attr->has.id)
    ZXIO_NODE_ATTR_SET(*inout_zxio_attr, id, attr.id);
  if (inout_zxio_attr->has.content_size)
    ZXIO_NODE_ATTR_SET(*inout_zxio_attr, content_size, attr.content_size);
  if (inout_zxio_attr->has.storage_size)
    ZXIO_NODE_ATTR_SET(*inout_zxio_attr, storage_size, attr.storage_size);
  if (inout_zxio_attr->has.link_count)
    ZXIO_NODE_ATTR_SET(*inout_zxio_attr, link_count, attr.link_count);
  if (inout_zxio_attr->has.creation_time)
    ZXIO_NODE_ATTR_SET(*inout_zxio_attr, creation_time, attr.creation_time);
  if (inout_zxio_attr->has.modification_time)
    ZXIO_NODE_ATTR_SET(*inout_zxio_attr, modification_time, attr.modification_time);
}

template <typename ToIo1ModePermissions>
fio::wire::NodeAttributes ToNodeAttributes(zxio_node_attributes_t attr,
                                           ToIo1ModePermissions to_io1) {
  return fio::wire::NodeAttributes{
      .mode = ToIo1ModeFileType(attr.protocols) | to_io1(attr.abilities),
      .id = attr.has.id ? attr.id : fio::wire::kInoUnknown,
      .content_size = attr.content_size,
      .storage_size = attr.storage_size,
      .link_count = attr.link_count,
      .creation_time = attr.creation_time,
      .modification_time = attr.modification_time,
  };
}

// POSIX expects EBADF for access denied errors which comes from ZX_ERR_BAD_STATE;
// ZX_ERR_ACCESS_DENIED produces EACCES which should only be used for sockets.
zx_status_t map_status(zx_status_t status) {
  switch (status) {
    case ZX_ERR_ACCESS_DENIED:
      return ZX_ERR_BAD_HANDLE;
  }
  return status;
}

template <typename Protocol, zxio_object_type_t kObjectType>
class Remote : public HasIo {
 protected:
  Remote(fidl::ClientEnd<Protocol> client_end, const zxio_ops_t& ops)
      : HasIo(ops), client_(std::move(client_end)) {}

  zx_status_t Close(const bool should_wait) {
    auto cleanup = fit::defer([this] { this->~Remote(); });

    if (client_.is_valid() && should_wait) {
      const fidl::WireResult result = client_->Close();
      if (!result.ok()) {
        return result.status();
      }
      const auto& response = result.value();
      if (response.is_error()) {
        return response.error_value();
      }
    }
    return ZX_OK;
  }

  zx_status_t Release(zx_handle_t* out_handle) {
    *out_handle = client_.TakeClientEnd().TakeChannel().release();
    return ZX_OK;
  }

  zx_status_t Borrow(zx_handle_t* out_handle) {
    *out_handle = client_.client_end().channel().get();
    return ZX_OK;
  }

  zx_status_t Clone(zx_handle_t* out_handle) {
    auto [client_end, server_end] = fidl::Endpoints<fio::Node>::Create();
    const fidl::Status result =
        client()->Clone(fio::wire::OpenFlags::kCloneSameRights, std::move(server_end));
    if (!result.ok()) {
      return result.status();
    }
    *out_handle = client_end.TakeChannel().release();
    return ZX_OK;
  }

  zx_status_t Sync();

  zx_status_t AttrGet(zxio_node_attributes_t* inout_attr);

  zx_status_t AttrSet(const zxio_node_attributes_t* attr);

  zx_status_t AdvisoryLock(advisory_lock_req* req);

  zx_status_t Readv(const zx_iovec_t* vector, size_t vector_count, zxio_flags_t flags,
                    size_t* out_actual);

  zx_status_t ReadvAt(zx_off_t offset, const zx_iovec_t* vector, size_t vector_count,
                      zxio_flags_t flags, size_t* out_actual);

  zx_status_t Writev(const zx_iovec_t* vector, size_t vector_count, zxio_flags_t flags,
                     size_t* out_actual);

  zx_status_t WritevAt(zx_off_t offset, const zx_iovec_t* vector, size_t vector_count,
                       zxio_flags_t flags, size_t* out_actual);

  zx_status_t Seek(zxio_seek_origin_t start, int64_t offset, size_t* out_offset);

  zx_status_t Truncate(uint64_t length);

  zx_status_t FlagsGet(uint32_t* out_flags);

  zx_status_t FlagsSet(uint32_t flags);

  zx_status_t VmoGet(zxio_vmo_flags_t zxio_flags, zx_handle_t* out_vmo);

  zx_status_t Open(uint32_t flags, const char* path, size_t path_len, zxio_storage_t* storage);

  zx_status_t Open3(const char* path, size_t path_len, zxio_open_flags_t flags,
                    const zxio_open_options_t* options, zxio_storage_t* storage);

  zx_status_t OpenAsync(uint32_t flags, const char* path, size_t path_len, zx_handle_t request);

  zx_status_t Unlink(const char* name, size_t name_len, int flags);

  zx_status_t TokenGet(zx_handle_t* out_token);

  zx_status_t Rename(const char* old_path, size_t old_path_len, zx_handle_t dst_token,
                     const char* new_path, size_t new_path_len);

  zx_status_t Link(const char* src_path, size_t src_path_len, zx_handle_t dst_token,
                   const char* dst_path, size_t dst_path_len);

  zx_status_t LinkInto(zx_handle_t dst_token, const char* dst_path, size_t dst_path_len);

  zx_status_t DirentIteratorInit(zxio_dirent_iterator_t* iterator);

  zx_status_t DirentIteratorNext(zxio_dirent_iterator_t* iterator, zxio_dirent_t* inout_entry);

  zx_status_t DirentIteratorRewind(zxio_dirent_iterator_t* iterator);

  void DirentIteratorDestroy(zxio_dirent_iterator_t* iterator);

  zx_status_t GetWindowSize(uint32_t* width, uint32_t* height);

  zx_status_t SetWindowSize(uint32_t width, uint32_t height);

  zx_status_t XattrList(void (*callback)(void* context, const uint8_t* name, size_t name_len),
                        void* context);

  zx_status_t XattrGet(const uint8_t* name, size_t name_len,
                       zx_status_t (*callback)(void* context, zxio_xattr_data_t data),
                       void* context);

  zx_status_t XattrSet(const uint8_t* name, size_t name_len, const uint8_t* value, size_t value_len,
                       zxio_xattr_set_mode_t mode);

  zx_status_t XattrRemove(const uint8_t* name, size_t name_len);

  zx_status_t Allocate(uint64_t offset, uint64_t len, zxio_allocate_mode_t mode);

  const fidl::WireSyncClient<Protocol>& client() const { return client_; }

 private:
  fidl::WireSyncClient<Protocol> client_;
};

class Pty : public Remote<fuchsia_hardware_pty::Device, ZXIO_OBJECT_TYPE_TTY> {
 public:
  Pty(fidl::ClientEnd<fuchsia_hardware_pty::Device> client_end, zx::eventpair event)
      : Remote(std::move(client_end), kOps), event_(std::move(event)) {}

  zx_status_t Close(const bool should_wait) {
    const zx_status_t status = Remote::Close(should_wait);
    this->~Pty();
    return status;
  }

  zx_status_t Clone(zx_handle_t* out_handle) {
    auto [client_end, server_end] = fidl::Endpoints<fuchsia_unknown::Cloneable>::Create();
    const fidl::Status result = client()->Clone2(std::move(server_end));
    if (!result.ok()) {
      return result.status();
    }
    *out_handle = client_end.TakeChannel().release();
    return ZX_OK;
  }

  void WaitBegin(zxio_signals_t zxio_signals, zx_handle_t* out_handle,
                 zx_signals_t* out_zx_signals) {
    *out_handle = event_.get();

    zx_signals_t zx_signals = ZX_SIGNAL_NONE;
    zx_signals |= [zxio_signals]() {
      fdevice::wire::DeviceSignal signals;
      if (zxio_signals & ZXIO_SIGNAL_READABLE) {
        signals |= fdevice::wire::DeviceSignal::kReadable;
      }
      if (zxio_signals & ZXIO_SIGNAL_OUT_OF_BAND) {
        signals |= fdevice::wire::DeviceSignal::kOob;
      }
      if (zxio_signals & ZXIO_SIGNAL_WRITABLE) {
        signals |= fdevice::wire::DeviceSignal::kWritable;
      }
      if (zxio_signals & ZXIO_SIGNAL_ERROR) {
        signals |= fdevice::wire::DeviceSignal::kError;
      }
      if (zxio_signals & ZXIO_SIGNAL_PEER_CLOSED) {
        signals |= fdevice::wire::DeviceSignal::kHangup;
      }
      return static_cast<zx_signals_t>(signals);
    }();
    if (zxio_signals & ZXIO_SIGNAL_READ_DISABLED) {
      zx_signals |= ZX_CHANNEL_PEER_CLOSED;
    }
    *out_zx_signals = zx_signals;
  }

  void WaitEnd(zx_signals_t zx_signals, zxio_signals_t* out_zxio_signals) {
    zxio_signals_t zxio_signals = ZXIO_SIGNAL_NONE;
    [&zxio_signals, signals = fdevice::wire::DeviceSignal::TruncatingUnknown(zx_signals)]() {
      if (signals & fdevice::wire::DeviceSignal::kReadable) {
        zxio_signals |= ZXIO_SIGNAL_READABLE;
      }
      if (signals & fdevice::wire::DeviceSignal::kOob) {
        zxio_signals |= ZXIO_SIGNAL_OUT_OF_BAND;
      }
      if (signals & fdevice::wire::DeviceSignal::kWritable) {
        zxio_signals |= ZXIO_SIGNAL_WRITABLE;
      }
      if (signals & fdevice::wire::DeviceSignal::kError) {
        zxio_signals |= ZXIO_SIGNAL_ERROR;
      }
      if (signals & fdevice::wire::DeviceSignal::kHangup) {
        zxio_signals |= ZXIO_SIGNAL_PEER_CLOSED;
      }
    }();
    if (zx_signals & ZX_CHANNEL_PEER_CLOSED) {
      zxio_signals |= ZXIO_SIGNAL_READ_DISABLED;
    }
    *out_zxio_signals = zxio_signals;
  }

  zx_status_t IsAtty(bool* tty);

 private:
  static const zxio_ops_t kOps;

  const zx::eventpair event_;
};

constexpr zxio_ops_t Pty::kOps = ([]() {
  using Adaptor = Adaptor<Pty>;
  zxio_ops_t ops = zxio_default_ops;
  ops.close = Adaptor::From<&Pty::Close>;
  ops.release = Adaptor::From<&Pty::Release>;
  ops.borrow = Adaptor::From<&Pty::Borrow>;
  ops.clone = Adaptor::From<&Pty::Clone>;

  ops.wait_begin = Adaptor::From<&Pty::WaitBegin>;
  ops.wait_end = Adaptor::From<&Pty::WaitEnd>;
  ops.readv = Adaptor::From<&Pty::Readv>;
  ops.writev = Adaptor::From<&Pty::Writev>;

  ops.isatty = Adaptor::From<&Pty::IsAtty>;
  ops.get_window_size = Adaptor::From<&Pty::GetWindowSize>;
  ops.set_window_size = Adaptor::From<&Pty::SetWindowSize>;
  return ops;
})();

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::Sync() {
  const fidl::WireResult result = client()->Sync();
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  return ZX_OK;
}

constexpr fio::NodeAttributesQuery BuildAttributeQuery(
    const zxio_node_attributes_t::zxio_node_attr_has_t& attr_has) {
  fio::NodeAttributesQuery query;

  if (attr_has.protocols)
    query |= fio::NodeAttributesQuery::kProtocols;
  if (attr_has.abilities)
    query |= fio::NodeAttributesQuery::kAbilities;
  if (attr_has.content_size)
    query |= fio::NodeAttributesQuery::kContentSize;
  if (attr_has.storage_size)
    query |= fio::NodeAttributesQuery::kStorageSize;
  if (attr_has.link_count)
    query |= fio::NodeAttributesQuery::kLinkCount;
  if (attr_has.id)
    query |= fio::NodeAttributesQuery::kId;
  if (attr_has.creation_time)
    query |= fio::NodeAttributesQuery::kCreationTime;
  if (attr_has.modification_time)
    query |= fio::NodeAttributesQuery::kModificationTime;
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  if (attr_has.change_time)
    query |= fio::NodeAttributesQuery::kChangeTime;
  if (attr_has.access_time)
    query |= fio::NodeAttributesQuery::kAccessTime;
  if (attr_has.mode)
    query |= fio::NodeAttributesQuery::kMode;
  if (attr_has.uid)
    query |= fio::NodeAttributesQuery::kUid;
  if (attr_has.gid)
    query |= fio::NodeAttributesQuery::kGid;
  if (attr_has.rdev)
    query |= fio::NodeAttributesQuery::kRdev;
#endif
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (attr_has.fsverity_options)
    query |= fio::NodeAttributesQuery::kOptions;
  if (attr_has.fsverity_root_hash)
    query |= fio::NodeAttributesQuery::kRootHash;
  if (attr_has.fsverity_enabled)
    query |= fio::NodeAttributesQuery::kVerityEnabled;
  if (attr_has.casefold)
    query |= fio::NodeAttributesQuery::kCasefold;
  if (attr_has.wrapping_key_id)
    query |= fio::NodeAttributesQuery::kWrappingKeyId;
#endif
  return query;
}

zx::result<fio::wire::MutableNodeAttributes> BuildMutableAttributes(
    const zxio_node_attributes_t* mutable_attrs,
    fidl::WireTableFrame<fio::wire::MutableNodeAttributes>& mutable_attrs_frame) {
  auto builder = fio::wire::MutableNodeAttributes::ExternalBuilder(
      fidl::ObjectView<fidl::WireTableFrame<fio::wire::MutableNodeAttributes>>::FromExternal(
          &mutable_attrs_frame));

  // Ensure no immutable attributes were specified.
  if (mutable_attrs->has.protocols || mutable_attrs->has.abilities || mutable_attrs->has.id ||
      mutable_attrs->has.content_size || mutable_attrs->has.storage_size ||
      mutable_attrs->has.link_count) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (mutable_attrs->has.change_time || mutable_attrs->has.fsverity_enabled ||
      mutable_attrs->has.fsverity_options || mutable_attrs->has.fsverity_root_hash) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
#endif
  if (mutable_attrs->has.creation_time) {
    builder.creation_time(fidl::ObjectView<uint64_t>::FromExternal(
        const_cast<uint64_t*>(&mutable_attrs->creation_time)));
  }
  if (mutable_attrs->has.modification_time) {
    builder.modification_time(fidl::ObjectView<uint64_t>::FromExternal(
        const_cast<uint64_t*>(&mutable_attrs->modification_time)));
  }
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  if (mutable_attrs->has.access_time) {
    builder.access_time(fidl::ObjectView<uint64_t>::FromExternal(
        const_cast<uint64_t*>(&mutable_attrs->access_time)));
  }
  if (mutable_attrs->has.mode) {
    builder.mode(mutable_attrs->mode);
  }
  if (mutable_attrs->has.uid) {
    builder.uid(mutable_attrs->uid);
  }
  if (mutable_attrs->has.gid) {
    builder.gid(mutable_attrs->gid);
  }
  if (mutable_attrs->has.rdev) {
    builder.rdev(
        fidl::ObjectView<uint64_t>::FromExternal(const_cast<uint64_t*>(&mutable_attrs->rdev)));
  }
#endif
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (mutable_attrs->has.casefold) {
    builder.casefold(mutable_attrs->casefold);
  }
  if (mutable_attrs->has.wrapping_key_id) {
    builder.wrapping_key_id(
        fidl::ObjectView<fidl::Array<uint8_t, ZXIO_WRAPPING_KEY_ID_LENGTH>>::FromExternal(
            reinterpret_cast<fidl::Array<uint8_t, ZXIO_WRAPPING_KEY_ID_LENGTH>*>(
                const_cast<uint8_t*>(mutable_attrs->wrapping_key_id))));
  }
#endif
  return zx::ok(builder.Build());
}

template <typename Protocol, typename ToZxioAbilities>
zx_status_t AttrGetCommon(const fidl::WireSyncClient<Protocol>& client, ToZxioAbilities to_zxio,
                          zxio_node_attributes_t* inout_attr) {
  const fidl::WireResult result = client->GetAttr();
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (const zx_status_t status = response.s; status != ZX_OK) {
    return status;
  }
  ToZxioNodeAttributes(response.attributes, to_zxio, inout_attr);
  return ZX_OK;
}

template <typename Protocol>
zx_status_t AttributesGetCommon(const fidl::WireSyncClient<Protocol>& client,
                                zxio_node_attributes_t* inout_attr) {
  // Construct query from has in inout_attr
  const fio::NodeAttributesQuery query = BuildAttributeQuery(inout_attr->has);
  const fidl::WireResult result = client->GetAttributes(query);
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  const fio::wire::NodeAttributes2* attributes = response.value();
  if (zx_status_t status = zxio_attr_from_wire(*attributes, inout_attr); status != ZX_OK)
    return status;
  return ZX_OK;
}

template <typename Protocol, typename ToIo1ModePermissions>
zx_status_t AttrSetCommon(const fidl::WireSyncClient<Protocol>& client, ToIo1ModePermissions to_io1,
                          const zxio_node_attributes_t* attr) {
  fio::wire::NodeAttributeFlags flags;
  zxio_node_attributes_t::zxio_node_attr_has_t remaining = attr->has;
  if (attr->has.creation_time) {
    flags |= fio::wire::NodeAttributeFlags::kCreationTime;
    remaining.creation_time = false;
  }
  if (attr->has.modification_time) {
    flags |= fio::wire::NodeAttributeFlags::kModificationTime;
    remaining.modification_time = false;
  }
  constexpr zxio_node_attributes_t::zxio_node_attr_has_t all_absent = {};
  if (remaining != all_absent) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  const fidl::WireResult result = client->SetAttr(flags, ToNodeAttributes(*attr, to_io1));
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  return response.s;
}

template <typename Protocol>
zx_status_t AttributesSetCommon(const fidl::WireSyncClient<Protocol>& client,
                                const zxio_node_attributes_t* attr) {
  fidl::WireTableFrame<fio::wire::MutableNodeAttributes> mutable_attrs_frame;
  const zx::result mutable_attributes = BuildMutableAttributes(attr, mutable_attrs_frame);
  if (mutable_attributes.is_error()) {
    return mutable_attributes.error_value();
  }
  const fidl::WireResult result = client->UpdateAttributes(*mutable_attributes);
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::AttrGet(zxio_node_attributes_t* inout_attr) {
  if (inout_attr->has.object_type) {
    ZXIO_NODE_ATTR_SET(*inout_attr, object_type, kObjectType);
  }
  // If any of the attributes that exist only in io2 are requested, we call GetAttributes (io2)
  if (inout_attr->has.mode || inout_attr->has.uid || inout_attr->has.gid || inout_attr->has.rdev ||
      inout_attr->has.access_time || inout_attr->has.change_time ||
      inout_attr->has.fsverity_options || inout_attr->has.fsverity_root_hash ||
      inout_attr->has.fsverity_enabled) {
    return AttributesGetCommon(client(), inout_attr);
  }
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (inout_attr->has.wrapping_key_id) {
    return AttributesGetCommon(client(), inout_attr);
  }
#endif
  return AttrGetCommon(client(), ToZxioAbilitiesForFile(), inout_attr);
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::AttrSet(const zxio_node_attributes_t* attr) {
  // If these attributes are set, call `update_attributes` (io2) otherwise, we can fall back to
  // `SetAttr` to only update creation and modification time.
  if (attr->has.mode || attr->has.uid || attr->has.gid || attr->has.rdev || attr->has.access_time) {
    return AttributesSetCommon(client(), attr);
  }
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (attr->has.wrapping_key_id) {
    return AttributesSetCommon(client(), attr);
  }
#endif
  return AttrSetCommon(client(), ToIo1ModePermissionsForFile(), attr);
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::AdvisoryLock(advisory_lock_req* req) {
  fio::wire::AdvisoryLockType lock_type;
  switch (req->type) {
    case ADVISORY_LOCK_SHARED:
      lock_type = fio::wire::AdvisoryLockType::kRead;
      break;
    case ADVISORY_LOCK_EXCLUSIVE:
      lock_type = fio::wire::AdvisoryLockType::kWrite;
      break;
    case ADVISORY_LOCK_UNLOCK:
      lock_type = fio::wire::AdvisoryLockType::kUnlock;
      break;
    default:
      return ZX_ERR_INTERNAL;
  }
  fidl::Arena allocator;
  const fidl::WireResult result = client()->AdvisoryLock(
      fio::wire::AdvisoryLockRequest::Builder(allocator).type(lock_type).wait(req->wait).Build());
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  return ZX_OK;
}

template <typename F>
zx_status_t zxio_remote_do_vector(const zx_iovec_t* vector, size_t vector_count, zxio_flags_t flags,
                                  size_t* out_actual, F fn) {
  return zxio_stream_do_vector(vector, vector_count, out_actual,
                               [&](void* data, size_t capacity, size_t* out_actual) {
                                 auto buffer = static_cast<uint8_t*>(data);
                                 size_t total = 0;
                                 while (capacity > 0) {
                                   const size_t chunk = std::min(capacity, fio::wire::kMaxBuf);
                                   size_t actual;
                                   const zx_status_t status = fn(buffer, chunk, &actual);
                                   if (status != ZX_OK) {
                                     if (total > 0) {
                                       break;
                                     }
                                     return status;
                                   }
                                   total += actual;
                                   if (actual != chunk) {
                                     break;
                                   }
                                   buffer += actual;
                                   capacity -= actual;
                                 }
                                 *out_actual = total;
                                 return ZX_OK;
                               });
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::Readv(const zx_iovec_t* vector, size_t vector_count,
                                                 zxio_flags_t flags, size_t* out_actual) {
  if (flags) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  return zxio_remote_do_vector(vector, vector_count, flags, out_actual,
                               [this](uint8_t* buffer, size_t capacity, size_t* out_actual) {
                                 // Explicitly allocating message buffers to avoid heap allocation.
                                 fidl::SyncClientBuffer<fio::File::Read> fidl_buffer;
                                 const fidl::WireUnownedResult result =
                                     client().buffer(fidl_buffer.view())->Read(capacity);
                                 if (!result.ok()) {
                                   return result.status();
                                 }
                                 const auto& response = result.value();
                                 if (response.is_error()) {
                                   return response.error_value();
                                 }
                                 const fidl::VectorView data = response.value()->data;
                                 const size_t actual = data.count();
                                 if (actual > capacity) {
                                   return ZX_ERR_IO;
                                 }
                                 memcpy(buffer, data.begin(), actual);
                                 *out_actual = actual;
                                 return ZX_OK;
                               });
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::ReadvAt(zx_off_t offset, const zx_iovec_t* vector,
                                                   size_t vector_count, zxio_flags_t flags,
                                                   size_t* out_actual) {
  if (flags) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  return zxio_remote_do_vector(
      vector, vector_count, flags, out_actual,
      [this, &offset](uint8_t* buffer, size_t capacity, size_t* out_actual) {
        // Explicitly allocating message buffers to avoid heap allocation.
        fidl::SyncClientBuffer<fio::File::ReadAt> fidl_buffer;
        const fidl::WireUnownedResult result =
            client().buffer(fidl_buffer.view())->ReadAt(capacity, offset);
        if (!result.ok()) {
          return result.status();
        }
        const auto& response = result.value();
        if (response.is_error()) {
          return response.error_value();
        }
        const fidl::VectorView data = response.value()->data;
        const size_t actual = data.count();
        if (actual > capacity) {
          return ZX_ERR_IO;
        }
        offset += actual;
        memcpy(buffer, data.begin(), actual);
        *out_actual = actual;
        return ZX_OK;
      });
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::Writev(const zx_iovec_t* vector, size_t vector_count,
                                                  zxio_flags_t flags, size_t* out_actual) {
  if (flags) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  return zxio_remote_do_vector(
      vector, vector_count, flags, out_actual,
      [this](uint8_t* buffer, size_t capacity, size_t* out_actual) {
        // Explicitly allocating message buffers to avoid heap allocation.
        fidl::SyncClientBuffer<fio::File::Write> fidl_buffer;
        const fidl::WireUnownedResult result =
            client()
                .buffer(fidl_buffer.view())
                ->Write(fidl::VectorView<uint8_t>::FromExternal(buffer, capacity));
        if (!result.ok()) {
          return result.status();
        }
        const auto& response = result.value();
        if (response.is_error()) {
          return response.error_value();
        }
        const size_t actual = response.value()->actual_count;
        if (actual > capacity) {
          return ZX_ERR_IO;
        }
        *out_actual = actual;
        return ZX_OK;
      });
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::WritevAt(zx_off_t offset, const zx_iovec_t* vector,
                                                    size_t vector_count, zxio_flags_t flags,
                                                    size_t* out_actual) {
  if (flags) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  return zxio_remote_do_vector(
      vector, vector_count, flags, out_actual,
      [this, &offset](uint8_t* buffer, size_t capacity, size_t* out_actual) {
        // Explicitly allocating message buffers to avoid heap allocation.
        fidl::SyncClientBuffer<fio::File::WriteAt> fidl_buffer;
        const fidl::WireUnownedResult result =
            client()
                .buffer(fidl_buffer.view())
                ->WriteAt(fidl::VectorView<uint8_t>::FromExternal(buffer, capacity), offset);
        if (!result.ok()) {
          return result.status();
        }
        const auto& response = result.value();
        if (response.is_error()) {
          return response.error_value();
        }
        const size_t actual = response.value()->actual_count;
        if (actual > capacity) {
          return ZX_ERR_IO;
        }
        offset += actual;
        *out_actual = actual;
        return ZX_OK;
      });
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::Seek(zxio_seek_origin_t start, int64_t offset,
                                                size_t* out_offset) {
  const fidl::WireResult result = client()->Seek(static_cast<fio::wire::SeekOrigin>(start), offset);
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  *out_offset = response.value()->offset_from_start;
  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::Truncate(uint64_t length) {
  const fidl::WireResult result = client()->Resize(length);
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::FlagsGet(uint32_t* out_flags) {
  const fidl::WireResult result = client()->GetFlags();
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (const zx_status_t status = response.s; status != ZX_OK) {
    return status;
  }
  *out_flags = static_cast<uint32_t>(response.flags);
  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::FlagsSet(uint32_t flags) {
  const fidl::WireResult result = client()->SetFlags(static_cast<fio::wire::OpenFlags>(flags));
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  return response.s;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::VmoGet(zxio_vmo_flags_t zxio_flags,
                                                  zx_handle_t* out_vmo) {
  fio::wire::VmoFlags flags;
  if (zxio_flags & ZXIO_VMO_READ) {
    flags |= fio::wire::VmoFlags::kRead;
  }
  if (zxio_flags & ZXIO_VMO_WRITE) {
    flags |= fio::wire::VmoFlags::kWrite;
  }
  if (zxio_flags & ZXIO_VMO_EXECUTE) {
    flags |= fio::wire::VmoFlags::kExecute;
  }
  if (zxio_flags & ZXIO_VMO_PRIVATE_CLONE) {
    flags |= fio::wire::VmoFlags::kPrivateClone;
  }
  if (zxio_flags & ZXIO_VMO_SHARED_BUFFER) {
    flags |= fio::wire::VmoFlags::kSharedBuffer;
  }
  fidl::WireResult result = client()->GetBackingMemory(flags);
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  zx::vmo& vmo = response.value()->vmo;
  *out_vmo = vmo.release();
  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::Open(uint32_t flags, const char* path, size_t path_len,
                                                zxio_storage_t* storage) {
  auto [client_end, server_end] = fidl::Endpoints<fio::Node>::Create();
  const fidl::Status result =
      client()->Open(static_cast<fio::wire::OpenFlags>(flags) | fio::wire::OpenFlags::kDescribe, {},
                     fidl::StringView::FromExternal(path, path_len), std::move(server_end));
  if (!result.ok()) {
    return result.status();
  }
  return zxio_create_with_on_open(client_end.TakeChannel().release(), storage);
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::Open3(const char* path, size_t path_len,
                                                 zxio_open_flags_t flags,
                                                 const zxio_open_options_t* options,
                                                 zxio_storage_t* storage) {
  zx::channel client_end, server_end;
  if (zx_status_t status = zx::channel::create(0, &client_end, &server_end); status != ZX_OK) {
    return status;
  }

  fio::NodeAttributesQuery attributes;
  fidl::WireTableFrame<fio::wire::MutableNodeAttributes> create_attributes_frame;
  zx::result<fio::wire::MutableNodeAttributes> create_attributes;
  fidl::WireTableFrame<fio::wire::Options> options_frame;
  fio::wire::Options open_options;

  if (options && (options->inout_attr || options->create_attr)) {
    auto options_builder = fio::wire::Options::ExternalBuilder(
        fidl::ObjectView<fidl::WireTableFrame<fio::wire::Options>>::FromExternal(&options_frame));

    // -- attributes --
    if (options->inout_attr) {
      attributes = BuildAttributeQuery(options->inout_attr->has);
      if (attributes) {
        options_builder.attributes(
            fidl::ObjectView<fio::wire::NodeAttributesQuery>::FromExternal(&attributes));
      }
    }

    // -- create_attributes --
    if (options->create_attr) {
      create_attributes = BuildMutableAttributes(options->create_attr, create_attributes_frame);
      if (create_attributes.is_error()) {
        return create_attributes.error_value();
      }
      options_builder.create_attributes(
          fidl::ObjectView<fio::wire::MutableNodeAttributes>::FromExternal(
              &create_attributes.value()));
    }

    open_options = options_builder.Build();
  }

  const fidl::Status result = client()->Open3(
      fidl::StringView::FromExternal(path, path_len),
      fio::Flags::kFlagSendRepresentation | fio::Flags{flags}, open_options, std::move(server_end));

  if (!result.ok()) {
    return result.status();
  }
  return zxio_create_with_on_representation(client_end.release(),
                                            options ? options->inout_attr : nullptr, storage);
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::OpenAsync(uint32_t flags, const char* path,
                                                     size_t path_len, zx_handle_t request) {
  fidl::ServerEnd<fio::Node> node_request{zx::channel(request)};
  const fidl::Status result =
      client()->Open(static_cast<fio::wire::OpenFlags>(flags), {},
                     fidl::StringView::FromExternal(path, path_len), std::move(node_request));
  return result.status();
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::Unlink(const char* name, size_t name_len, int flags) {
  fidl::Arena allocator;
  auto options = fio::wire::UnlinkOptions::Builder(allocator);
  auto io_flags = fio::wire::UnlinkFlags::kMustBeDirectory;
  if (flags & AT_REMOVEDIR) {
    options.flags(fidl::ObjectView<decltype(io_flags)>::FromExternal(&io_flags));
  }
  const fidl::WireResult result =
      client()->Unlink(fidl::StringView::FromExternal(name, name_len), options.Build());
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::TokenGet(zx_handle_t* out_token) {
  fidl::WireResult result = client()->GetToken();
  if (!result.ok()) {
    return result.status();
  }
  auto& response = result.value();
  if (const zx_status_t status = response.s; status != ZX_OK) {
    return status;
  }
  *out_token = response.token.release();
  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::Rename(const char* old_path, size_t old_path_len,
                                                  zx_handle_t dst_token, const char* new_path,
                                                  size_t new_path_len) {
  const fidl::WireResult result =
      client()->Rename(fidl::StringView::FromExternal(old_path, old_path_len), zx::event(dst_token),
                       fidl::StringView::FromExternal(new_path, new_path_len));
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::Link(const char* src_path, size_t src_path_len,
                                                zx_handle_t dst_token, const char* dst_path,
                                                size_t dst_path_len) {
  const fidl::WireResult result =
      client()->Link(fidl::StringView::FromExternal(src_path, src_path_len), zx::handle(dst_token),
                     fidl::StringView::FromExternal(dst_path, dst_path_len));
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  return response.s;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::LinkInto(zx_handle_t dst_token_handle,
                                                    const char* dst_path, size_t dst_path_len) {
  zx::event dst_token(dst_token_handle);
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  const fidl::WireResult result = client()->LinkInto(
      std::move(dst_token), fidl::StringView::FromExternal(dst_path, dst_path_len));
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  return ZX_OK;
#else
  return ZX_ERR_NOT_SUPPORTED;
#endif  // FUCHSIA_API_LEVEL_AT_LEAST(18)
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::DirentIteratorInit(zxio_dirent_iterator_t* iterator) {
  new (iterator) DirentIteratorImpl(io());
  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::DirentIteratorNext(zxio_dirent_iterator_t* iterator,
                                                              zxio_dirent_t* inout_entry) {
  return reinterpret_cast<DirentIteratorImpl*>(iterator)->Next(inout_entry);
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::DirentIteratorRewind(zxio_dirent_iterator_t* iterator) {
  return reinterpret_cast<DirentIteratorImpl*>(iterator)->Rewind();
}

template <typename Protocol, zxio_object_type_t kObjectType>
void Remote<Protocol, kObjectType>::DirentIteratorDestroy(zxio_dirent_iterator_t* iterator) {
  reinterpret_cast<DirentIteratorImpl*>(iterator)->~DirentIteratorImpl();
}

zx_status_t Pty::IsAtty(bool* tty) {
  *tty = true;
  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::GetWindowSize(uint32_t* width, uint32_t* height) {
  if (!client().is_valid()) {
    return ZX_ERR_BAD_STATE;
  }
  const fidl::WireResult result = client()->GetWindowSize();
  if (!result.ok()) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  const auto& response = result.value();
  if (response.status != ZX_OK) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  *width = response.size.width;
  *height = response.size.height;
  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::SetWindowSize(uint32_t width, uint32_t height) {
  if (!client().is_valid()) {
    return ZX_ERR_BAD_STATE;
  }

  const fuchsia_hardware_pty::wire::WindowSize size = {
      .width = width,
      .height = height,
  };

  const fidl::WireResult result = client()->SetWindowSize(size);
  if (!result.ok()) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  const auto& response = result.value();
  if (response.status != ZX_OK) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  return ZX_OK;
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::XattrList(
    void (*callback)(void* context, const uint8_t* name, size_t name_len), void* context) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  if (!client().is_valid()) {
    return ZX_ERR_BAD_STATE;
  }

  auto [client_end, server_end] = fidl::Endpoints<fuchsia_io::ExtendedAttributeIterator>::Create();
  const fidl::OneWayStatus result = client()->ListExtendedAttributes(std::move(server_end));
  if (!result.ok()) {
    return result.status();
  }

  while (true) {
    const fidl::WireResult next_result = fidl::WireCall(client_end)->GetNext();
    if (!next_result.ok()) {
      return next_result.status();
    }
    const auto& response = next_result.value();
    if (response.is_error()) {
      return response.error_value();
    }
    const fidl::VectorView data = response->attributes;
    for (fidl::VectorView<uint8_t>& name : data) {
      callback(context, name.data(), name.count());
    }
    if (response->last) {
      break;
    }
  }

  return ZX_OK;
#else
  return ZX_ERR_NOT_SUPPORTED;
#endif  // FUCHSIA_API_LEVEL_AT_LEAST(18)
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::XattrGet(const uint8_t* name, size_t name_len,
                                                    zx_status_t (*callback)(void* context,
                                                                            zxio_xattr_data_t data),
                                                    void* context) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  if (!client().is_valid()) {
    return ZX_ERR_BAD_STATE;
  }

  fidl::Arena arena;
  const fidl::WireResult result =
      client()->GetExtendedAttribute(fidl::VectorView<uint8_t>(arena, cpp20::span(name, name_len)));
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }

  const fuchsia_io::wire::ExtendedAttributeValue* value_resp = response.value();
  zxio_xattr_data_t data;
  if (value_resp->is_bytes()) {
    const fidl::VectorView value_bytes = value_resp->bytes();
    data = {
        .data = value_bytes.data(),
        .vmo = ZX_HANDLE_INVALID,
        .len = value_bytes.count(),
    };
  } else {
    const zx::vmo& value_vmo = value_resp->buffer();
    uint64_t value_size;
    if (zx_status_t status = value_vmo.get_prop_content_size(&value_size); status != ZX_OK) {
      return status;
    }
    data = {
        .data = nullptr,
        .vmo = value_vmo.get(),
        .len = value_size,
    };
  }

  return callback(context, data);
#else
  return ZX_ERR_NOT_SUPPORTED;
#endif  // FUCHSIA_API_LEVEL_AT_LEAST(18)
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::XattrSet(const uint8_t* name, size_t name_len,
                                                    const uint8_t* value, size_t value_len,
                                                    zxio_xattr_set_mode_t mode) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  if (!client().is_valid()) {
    return ZX_ERR_BAD_STATE;
  }

  fidl::Arena arena;
  fuchsia_io::wire::ExtendedAttributeValue attribute_value;
  if (value_len > fuchsia_io::wire::kMaxInlineAttributeValue) {
    zx::vmo val;
    if (zx_status_t status = zx::vmo::create(value_len, 0, &val); status != ZX_OK) {
      return status;
    }
    if (zx_status_t status = val.write(value, 0, value_len); status != ZX_OK) {
      return status;
    }
    attribute_value = fuchsia_io::wire::ExtendedAttributeValue::WithBuffer(std::move(val));
  } else {
    auto value_vec = fidl::VectorView<uint8_t>(arena, cpp20::span(value, value_len));
    fidl::ObjectView value_obj(arena, value_vec);
    attribute_value = fuchsia_io::wire::ExtendedAttributeValue::WithBytes(value_obj);
  }

  fio::SetExtendedAttributeMode mode_fidl;
  switch (mode) {
    case ZXIO_XATTR_SET:
      mode_fidl = fio::SetExtendedAttributeMode::kSet;
      break;
    case ZXIO_XATTR_CREATE:
      mode_fidl = fio::SetExtendedAttributeMode::kCreate;
      break;
    case ZXIO_XATTR_REPLACE:
      mode_fidl = fio::SetExtendedAttributeMode::kReplace;
      break;
    default:
      return ZX_ERR_INVALID_ARGS;
  }

  const fidl::WireResult result =
      client()->SetExtendedAttribute(fidl::VectorView<uint8_t>(arena, cpp20::span(name, name_len)),
                                     std::move(attribute_value), mode_fidl);
  if (!result.ok()) {
    return result.status();
  }
  if (result.value().is_error()) {
    return result.value().error_value();
  }

  return ZX_OK;
#else
  return ZX_ERR_NOT_SUPPORTED;
#endif  // FUCHSIA_API_LEVEL_AT_LEAST(18)
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::XattrRemove(const uint8_t* name, size_t name_len) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  if (!client().is_valid()) {
    return ZX_ERR_BAD_STATE;
  }

  fidl::Arena arena;
  const fidl::WireResult result = client()->RemoveExtendedAttribute(
      fidl::VectorView<uint8_t>(arena, cpp20::span(name, name_len)));
  if (!result.ok()) {
    return result.status();
  }
  if (result.value().is_error()) {
    return result.value().error_value();
  }

  return ZX_OK;
#else
  return ZX_ERR_NOT_SUPPORTED;
#endif  // FUCHSIA_API_LEVEL_AT_LEAST(18)
}

template <typename Protocol, zxio_object_type_t kObjectType>
zx_status_t Remote<Protocol, kObjectType>::Allocate(uint64_t offset, uint64_t len,
                                                    zxio_allocate_mode_t mode) {
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (!client().is_valid()) {
    return ZX_ERR_BAD_STATE;
  }

  fio::AllocateMode fidl_mode;
  if (mode & ZXIO_ALLOCATE_KEEP_SIZE) {
    fidl_mode |= fio::AllocateMode::kKeepSize;
  } else if (mode & ZXIO_ALLOCATE_UNSHARE_RANGE) {
    fidl_mode |= fio::AllocateMode::kUnshareRange;
  } else if (mode & ZXIO_ALLOCATE_PUNCH_HOLE) {
    fidl_mode |= fio::AllocateMode::kPunchHole;
  } else if (mode & ZXIO_ALLOCATE_COLLAPSE_RANGE) {
    fidl_mode |= fio::AllocateMode::kCollapseRange;
  } else if (mode & ZXIO_ALLOCATE_ZERO_RANGE) {
    fidl_mode |= fio::AllocateMode::kZeroRange;
  } else if (mode & ZXIO_ALLOCATE_INSERT_RANGE) {
    fidl_mode |= fio::AllocateMode::kInsertRange;
  }
  const fidl::WireResult result = client()->Allocate(offset, len, fidl_mode);
  if (!result.ok()) {
    return result.status();
  }
  if (result.value().is_error()) {
    return result.value().error_value();
  }

  return ZX_OK;
#else
  return ZX_ERR_NOT_SUPPORTED;
#endif  // FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
}

class Node : public Remote<fio::Node, ZXIO_OBJECT_TYPE_NODE> {
 public:
  explicit Node(fidl::ClientEnd<fio::Node> client_end) : Remote(std::move(client_end), kOps) {}

 private:
  static const zxio_ops_t kOps;
};

constexpr zxio_ops_t Node::kOps = ([]() {
  using Adaptor = Adaptor<Node>;
  zxio_ops_t ops = zxio_default_ops;
  ops.close = Adaptor::From<&Node::Close>;
  ops.release = Adaptor::From<&Node::Release>;
  ops.borrow = Adaptor::From<&Node::Borrow>;
  ops.clone = Adaptor::From<&Node::Clone>;

  ops.sync = Adaptor::From<&Node::Sync>;
  ops.attr_get = Adaptor::From<&Node::AttrGet>;
  ops.attr_set = Adaptor::From<&Node::AttrSet>;
  ops.flags_get = Adaptor::From<&Node::FlagsGet>;
  ops.flags_set = Adaptor::From<&Node::FlagsSet>;
  return ops;
})();

class Directory : public Remote<fio::Directory, ZXIO_OBJECT_TYPE_DIR> {
 public:
  explicit Directory(fidl::ClientEnd<fio::Directory> client_end)
      : Remote(std::move(client_end), kOps) {}

 private:
  friend class DirentIteratorImpl;

  zx_status_t Readv(const zx_iovec_t* vector, size_t vector_count, zxio_flags_t flags,
                    size_t* out_actual) {
    if (flags) {
      return ZX_ERR_NOT_SUPPORTED;
    }

    return zxio_do_vector(
        vector, vector_count, out_actual,
        [](void* buffer, size_t capacity, size_t total_so_far, size_t* out_actual) {
          if (capacity > 0) {
            return ZX_ERR_WRONG_TYPE;
          }
          *out_actual = 0;
          return ZX_OK;
        });
  }

  zx_status_t ReadvAt(zx_off_t offset, const zx_iovec_t* vector, size_t vector_count,
                      zxio_flags_t flags, size_t* out_actual) {
    return Readv(vector, vector_count, flags, out_actual);
  }

  zx_status_t AttrGet(zxio_node_attributes_t* inout_attr) {
    if (inout_attr->has.object_type) {
      ZXIO_NODE_ATTR_SET(*inout_attr, object_type, ZXIO_OBJECT_TYPE_DIR);
    }
    // If any of the attributes that exist only in io2 are requested, we call GetAttributes (io2)
    if (inout_attr->has.mode || inout_attr->has.uid || inout_attr->has.gid ||
        inout_attr->has.rdev || inout_attr->has.access_time || inout_attr->has.change_time ||
        inout_attr->has.casefold || inout_attr->has.wrapping_key_id) {
      return AttributesGetCommon(client(), inout_attr);
    }
    return AttrGetCommon(client(), ToZxioAbilitiesForDirectory(), inout_attr);
  }

  zx_status_t AttrSet(const zxio_node_attributes_t* attr) {
    // If these attributes are set, call `update_attributes` (io2) otherwise, we can fall back to
    // `SetAttr` to only update creation and modification time.
    if (attr->has.mode || attr->has.uid || attr->has.gid || attr->has.rdev ||
        attr->has.access_time || attr->has.casefold || attr->has.access_time ||
        attr->has.wrapping_key_id) {
      return AttributesSetCommon(client(), attr);
    }
    return AttrSetCommon(client(), ToIo1ModePermissionsForDirectory(), attr);
  }

  zx_status_t WatchDirectory(zxio_watch_directory_cb cb, zx_time_t deadline, void* context) {
    if (cb == nullptr) {
      return ZX_ERR_INVALID_ARGS;
    }
    zx::result endpoints = fidl::CreateEndpoints<fio::DirectoryWatcher>();
    if (endpoints.is_error()) {
      return endpoints.status_value();
    }

    const fidl::WireResult result =
        client()->Watch(fio::wire::WatchMask::kMask, 0, std::move(endpoints->server));

    if (!result.ok()) {
      return result.status();
    }
    const auto& response = result.value();
    if (const zx_status_t status = response.s; status != ZX_OK) {
      return status;
    }

    for (;;) {
      uint8_t bytes[fio::wire::kMaxBuf + 1];  // Extra byte for temporary null terminator.
      uint32_t num_bytes;
      zx_status_t status = endpoints->client.channel().read_etc(0, &bytes, nullptr, sizeof(bytes),
                                                                0, &num_bytes, nullptr);
      if (status != ZX_OK) {
        if (status == ZX_ERR_SHOULD_WAIT) {
          status = endpoints->client.channel().wait_one(
              ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED, zx::time(deadline), nullptr);
          if (status != ZX_OK) {
            return status;
          }
          continue;
        }
        return status;
      }

      // Message Format: { OP, LEN, DATA[LEN] }
      const cpp20::span span(bytes, num_bytes);
      auto it = span.begin();
      for (;;) {
        if (std::distance(it, span.end()) < 2) {
          break;
        }

        const fio::wire::WatchEvent wire_event = static_cast<fio::wire::WatchEvent>(*it++);
        const uint8_t len = *it++;
        uint8_t* name = &*it;

        if (std::distance(it, span.end()) < len) {
          break;
        }
        it += len;

        zxio_watch_directory_event_t event;
        switch (wire_event) {
          case fio::wire::WatchEvent::kAdded:
          case fio::wire::WatchEvent::kExisting:
            event = ZXIO_WATCH_EVENT_ADD_FILE;
            break;
          case fio::wire::WatchEvent::kRemoved:
            event = ZXIO_WATCH_EVENT_REMOVE_FILE;
            break;
          case fio::wire::WatchEvent::kIdle:
            event = ZXIO_WATCH_EVENT_WAITING;
            break;
          default:
            // unsupported event
            continue;
        }

        // The callback expects a null-terminated string.
        const uint8_t tmp = *it;
        *it = 0;
        status = cb(event, reinterpret_cast<const char*>(name), context);
        *it = tmp;
        if (status != ZX_OK) {
          return status;
        }
      }
    }
  }

  zx_status_t CreateSymlink(const char* name, size_t name_len, const uint8_t* target,
                            size_t target_len, zxio_storage_t* storage) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
    fidl::Endpoints<fio::Symlink> endpoints;
    if (storage) {
      if (auto result = fidl::CreateEndpoints<fio::Symlink>(); result.is_error()) {
        return result.status_value();
      } else {
        endpoints = *std::move(result);
      }
    }

    fidl::Arena arena;
    const fidl::WireResult result =
        client()->CreateSymlink(fidl::StringView(arena, std::string_view(name, name_len)),
                                fidl::VectorView<uint8_t>(arena, cpp20::span(target, target_len)),
                                std::move(endpoints.server));
    if (!result.ok()) {
      return result.status();
    }
    const auto& response = result.value();
    if (response.is_error()) {
      return response.error_value();
    }
    if (storage) {
      if (zx_status_t status = zxio_symlink_init(storage, std::move(endpoints.client),
                                                 std::vector(target, target + target_len));
          status != ZX_OK) {
        return status;
      }
    }
    return ZX_OK;
#else
    return ZX_ERR_NOT_SUPPORTED;
#endif  // FUCHSIA_API_LEVEL_AT_LEAST(18)
  }

  static const zxio_ops_t kOps;
};

const fidl::WireSyncClient<fio::Directory>& DirentIteratorImpl::client() const {
  return io_->client();
}

constexpr zxio_ops_t Directory::kOps = ([]() {
  using Adaptor = Adaptor<Directory>;
  zxio_ops_t ops = zxio_default_ops;
  ops.close = Adaptor::From<&Directory::Close>;
  ops.release = Adaptor::From<&Directory::Release>;
  ops.borrow = Adaptor::From<&Directory::Borrow>;
  ops.clone = Adaptor::From<&Directory::Clone>;

  // use specialized read functions that succeed for zero-sized reads.
  ops.readv = Adaptor::From<&Directory::Readv>;
  ops.readv_at = Adaptor::From<&Directory::ReadvAt>;

  ops.open = Adaptor::From<&Directory::Open>;
  ops.open3 = Adaptor::From<&Directory::Open3>;
  ops.open_async = Adaptor::From<&Directory::OpenAsync>;
  ops.unlink = Adaptor::From<&Directory::Unlink>;
  ops.token_get = Adaptor::From<&Directory::TokenGet>;
  ops.rename = Adaptor::From<&Directory::Rename>;
  ops.link = Adaptor::From<&Directory::Link>;
  ops.dirent_iterator_init = Adaptor::From<&Directory::DirentIteratorInit>;
  ops.dirent_iterator_next = Adaptor::From<&Directory::DirentIteratorNext>;
  ops.dirent_iterator_rewind = Adaptor::From<&Directory::DirentIteratorRewind>;
  ops.dirent_iterator_destroy = Adaptor::From<&Directory::DirentIteratorDestroy>;
  ops.watch_directory = Adaptor::From<&Directory::WatchDirectory>;

  ops.sync = Adaptor::From<&Directory::Sync>;
  ops.attr_get = Adaptor::From<&Directory::AttrGet>;
  ops.attr_set = Adaptor::From<&Directory::AttrSet>;
  ops.flags_get = Adaptor::From<&Directory::FlagsGet>;
  ops.flags_set = Adaptor::From<&Directory::FlagsSet>;
  ops.advisory_lock = Adaptor::From<&Directory::AdvisoryLock>;

  ops.create_symlink = Adaptor::From<&Directory::CreateSymlink>;
  ops.xattr_list = Adaptor::From<&Directory::XattrList>;
  ops.xattr_get = Adaptor::From<&Directory::XattrGet>;
  ops.xattr_set = Adaptor::From<&Directory::XattrSet>;
  ops.xattr_remove = Adaptor::From<&Directory::XattrRemove>;
  return ops;
})();

class File : public Remote<fio::File, ZXIO_OBJECT_TYPE_FILE> {
 public:
  File(fidl::ClientEnd<fio::File> client_end, zx::event event, zx::stream stream)
      : Remote(std::move(client_end), kOps), event_(std::move(event)), stream_(std::move(stream)) {}

 private:
  zx_status_t Close(const bool should_wait) {
    const zx_status_t status = Remote::Close(should_wait);
    this->~File();
    return status;
  }

  zx_status_t EnableVerity(const zxio_fsverity_descriptor_t* descriptor) {
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    fidl::WireTableFrame<fio::wire::VerificationOptions> verification_options_frame;
    fio::wire::VerificationOptions verification_options;
    auto builder = fio::wire::VerificationOptions::ExternalBuilder(
        fidl::ObjectView<fidl::WireTableFrame<fio::wire::VerificationOptions>>::FromExternal(
            &verification_options_frame));
    builder.hash_algorithm(static_cast<fio::wire::HashAlgorithm>(descriptor->hash_algorithm));
    fidl::VectorView<uint8_t> vec = fidl::VectorView<uint8_t>::FromExternal(
        const_cast<uint8_t*>(std::begin(descriptor->salt)), descriptor->salt_size);
    builder.salt(fidl::ObjectView<fidl::VectorView<uint8_t>>::FromExternal(&vec));
    verification_options = builder.Build();

    const fidl::WireResult result = client()->EnableVerity(verification_options);
    if (!result.ok()) {
      return result.status();
    }
    const auto& response = result.value();
    if (response.is_error()) {
      return response.error_value();
    }
    return ZX_OK;
#else
    return ZX_ERR_NOT_SUPPORTED;
#endif  // FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  }

  void WaitBegin(zxio_signals_t zxio_signals, zx_handle_t* out_handle,
                 zx_signals_t* out_zx_signals) {
    *out_handle = event_.get();

    zx_signals_t zx_signals = ZX_SIGNAL_NONE;
    if (zxio_signals & ZXIO_SIGNAL_READABLE) {
      zx_signals |= static_cast<zx_signals_t>(fio::wire::FileSignal::kReadable);
    }
    if (zxio_signals & ZXIO_SIGNAL_WRITABLE) {
      zx_signals |= static_cast<zx_signals_t>(fio::wire::FileSignal::kWritable);
    }
    *out_zx_signals = zx_signals;
  }

  void WaitEnd(zx_signals_t zx_signals, zxio_signals_t* out_zxio_signals) {
    zxio_signals_t zxio_signals = ZXIO_SIGNAL_NONE;
    if (zx_signals & static_cast<zx_signals_t>(fio::wire::FileSignal::kReadable)) {
      zxio_signals |= ZXIO_SIGNAL_READABLE;
    }
    if (zx_signals & static_cast<zx_signals_t>(fio::wire::FileSignal::kWritable)) {
      zxio_signals |= ZXIO_SIGNAL_WRITABLE;
    }
    *out_zxio_signals = zxio_signals;
  }

  zx_status_t Readv(const zx_iovec_t* vector, size_t vector_count, zxio_flags_t flags,
                    size_t* out_actual) {
    if (flags) {
      return ZX_ERR_NOT_SUPPORTED;
    }

    if (stream_.is_valid()) {
      return map_status(stream_.readv(0, vector, vector_count, out_actual));
    }
    return Remote::Readv(vector, vector_count, flags, out_actual);
  }

  zx_status_t ReadvAt(zx_off_t offset, const zx_iovec_t* vector, size_t vector_count,
                      zxio_flags_t flags, size_t* out_actual) {
    if (flags) {
      return ZX_ERR_NOT_SUPPORTED;
    }

    if (stream_.is_valid()) {
      return map_status(stream_.readv_at(0, offset, vector, vector_count, out_actual));
    }
    return Remote::ReadvAt(offset, vector, vector_count, flags, out_actual);
  }

  zx_status_t Writev(const zx_iovec_t* vector, size_t vector_count, zxio_flags_t flags,
                     size_t* out_actual) {
    if (flags) {
      return ZX_ERR_NOT_SUPPORTED;
    }

    if (stream_.is_valid()) {
      return map_status(stream_.writev(0, vector, vector_count, out_actual));
    }
    return Remote::Writev(vector, vector_count, flags, out_actual);
  }

  zx_status_t WritevAt(zx_off_t offset, const zx_iovec_t* vector, size_t vector_count,
                       zxio_flags_t flags, size_t* out_actual) {
    if (flags) {
      return ZX_ERR_NOT_SUPPORTED;
    }

    if (stream_.is_valid()) {
      return map_status(stream_.writev_at(0, offset, vector, vector_count, out_actual));
    }
    return Remote::WritevAt(offset, vector, vector_count, flags, out_actual);
  }

  zx_status_t Seek(zxio_seek_origin_t start, int64_t offset, size_t* out_offset) {
    if (stream_.is_valid()) {
      return map_status(stream_.seek(start, offset, out_offset));
    }
    return Remote::Seek(start, offset, out_offset);
  }

  static const zxio_ops_t kOps;

  const zx::event event_;
  const zx::stream stream_;
};

constexpr zxio_ops_t File::kOps = ([]() {
  using Adaptor = Adaptor<File>;
  zxio_ops_t ops = zxio_default_ops;
  ops.close = Adaptor::From<&File::Close>;
  ops.release = Adaptor::From<&File::Release>;
  ops.borrow = Adaptor::From<&File::Borrow>;
  ops.clone = Adaptor::From<&File::Clone>;

  ops.wait_begin = Adaptor::From<&File::WaitBegin>;
  ops.wait_end = Adaptor::From<&File::WaitEnd>;
  ops.readv = Adaptor::From<&File::Readv>;
  ops.readv_at = Adaptor::From<&File::ReadvAt>;
  ops.writev = Adaptor::From<&File::Writev>;
  ops.writev_at = Adaptor::From<&File::WritevAt>;
  ops.seek = Adaptor::From<&File::Seek>;
  ops.truncate = Adaptor::From<&File::Truncate>;
  ops.vmo_get = Adaptor::From<&File::VmoGet>;
  ops.enable_verity = Adaptor::From<&File::EnableVerity>;

  ops.sync = Adaptor::From<&File::Sync>;
  ops.attr_get = Adaptor::From<&File::AttrGet>;
  ops.attr_set = Adaptor::From<&File::AttrSet>;
  ops.flags_get = Adaptor::From<&File::FlagsGet>;
  ops.flags_set = Adaptor::From<&File::FlagsSet>;
  ops.advisory_lock = Adaptor::From<&File::AdvisoryLock>;

  ops.xattr_list = Adaptor::From<&File::XattrList>;
  ops.xattr_get = Adaptor::From<&File::XattrGet>;
  ops.xattr_set = Adaptor::From<&File::XattrSet>;
  ops.xattr_remove = Adaptor::From<&File::XattrRemove>;

  ops.link_into = Adaptor::From<&File::LinkInto>;
  ops.allocate = Adaptor::From<&File::Allocate>;

  return ops;
})();

#if FUCHSIA_API_LEVEL_AT_LEAST(18)

class Symlink : public Remote<fio::Symlink, ZXIO_OBJECT_TYPE_SYMLINK> {
 public:
  Symlink(fidl::ClientEnd<fio::Symlink> client_end, std::vector<uint8_t> target)
      : Remote(std::move(client_end), kOps), target_(std::move(target)) {}

 private:
  zx_status_t Close(const bool should_wait) {
    const zx_status_t status = Remote::Close(should_wait);
    this->~Symlink();
    return status;
  }

  zx_status_t ReadLink(const uint8_t** out_target, size_t* out_target_len) const {
    *out_target = target_.data();
    *out_target_len = target_.size();
    return ZX_OK;
  }

  static const zxio_ops_t kOps;
  const std::vector<uint8_t> target_;
};

constexpr zxio_ops_t Symlink::kOps = ([]() {
  using Adaptor = Adaptor<Symlink>;
  zxio_ops_t ops = zxio_default_ops;
  ops.close = Adaptor::From<&Symlink::Close>;
  ops.release = Adaptor::From<&Symlink::Release>;
  ops.borrow = Adaptor::From<&Symlink::Borrow>;
  ops.clone = Adaptor::From<&Symlink::Clone>;
  ops.attr_get = Adaptor::From<&Symlink::AttrGet>;
  ops.flags_get = Adaptor::From<&Symlink::FlagsGet>;
  ops.read_link = Adaptor::From<&Symlink::ReadLink>;
  ops.link_into = Adaptor::From<&Symlink::LinkInto>;
  ops.xattr_list = Adaptor::From<&Symlink::XattrList>;
  ops.xattr_get = Adaptor::From<&Symlink::XattrGet>;
  ops.xattr_set = Adaptor::From<&Symlink::XattrSet>;
  ops.xattr_remove = Adaptor::From<&Symlink::XattrRemove>;
  return ops;
})();

#endif  // FUCHSIA_API_LEVEL_AT_LEAST(18)

}  // namespace

zx_status_t zxio_dir_init(zxio_storage_t* storage, fidl::ClientEnd<fio::Directory> client) {
  new (storage) Directory(std::move(client));
  return ZX_OK;
}

zx_status_t zxio_file_init(zxio_storage_t* storage, zx::event event, zx::stream stream,
                           fidl::ClientEnd<fuchsia_io::File> client) {
  new (storage) File(std::move(client), std::move(event), std::move(stream));
  return ZX_OK;
}

zx_status_t zxio_node_init(zxio_storage_t* storage, fidl::ClientEnd<fio::Node> client) {
  new (storage) Node(std::move(client));
  return ZX_OK;
}

zx_status_t zxio_pty_init(zxio_storage_t* storage, zx::eventpair event,
                          fidl::ClientEnd<fuchsia_hardware_pty::Device> client) {
  new (storage) Pty(std::move(client), std::move(event));
  return ZX_OK;
}

#if FUCHSIA_API_LEVEL_AT_LEAST(18)
zx_status_t zxio_symlink_init(zxio_storage_t* storage, fidl::ClientEnd<fio::Symlink> client,
                              std::vector<uint8_t> target) {
  new (storage) Symlink(std::move(client), std::move(target));
  return ZX_OK;
}
#endif

uint32_t zxio_node_protocols_to_posix_type(zxio_node_protocols_t protocols) {
  return ToIo1ModeFileType(protocols);
}

__EXPORT
uint32_t zxio_get_posix_mode(zxio_node_protocols_t protocols, zxio_abilities_t abilities) {
  uint32_t mode = zxio_node_protocols_to_posix_type(protocols);
  if (mode & S_IFDIR) {
    mode |= ToIo1ModePermissionsForDirectory()(abilities);
  } else {
    mode |= ToIo1ModePermissionsForFile()(abilities);
  }
  return mode;
}

zx_status_t zxio_attr_from_wire(const fio::wire::NodeAttributes2& in, zxio_node_attributes_t* out) {
  out->has = {};

  if (in.immutable_attributes.has_protocols()) {
    out->protocols = static_cast<uint64_t>(in.immutable_attributes.protocols());
    out->has.protocols = true;
  }

  if (in.immutable_attributes.has_abilities()) {
    out->abilities = static_cast<uint64_t>(in.immutable_attributes.abilities());
    out->has.abilities = true;
  }

  if (in.immutable_attributes.has_id()) {
    out->id = in.immutable_attributes.id();
    out->has.id = true;
  }

  if (in.immutable_attributes.has_content_size()) {
    out->content_size = in.immutable_attributes.content_size();
    out->has.content_size = true;
  }

  if (in.immutable_attributes.has_storage_size()) {
    out->storage_size = in.immutable_attributes.storage_size();
    out->has.storage_size = true;
  }

  if (in.immutable_attributes.has_link_count()) {
    out->link_count = in.immutable_attributes.link_count();
    out->has.link_count = true;
  }

  if (in.mutable_attributes.has_creation_time()) {
    out->creation_time = in.mutable_attributes.creation_time();
    out->has.creation_time = true;
  }

  if (in.mutable_attributes.has_modification_time()) {
    out->modification_time = in.mutable_attributes.modification_time();
    out->has.modification_time = true;
  }
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  if (in.mutable_attributes.has_access_time()) {
    out->access_time = in.mutable_attributes.access_time();
    out->has.access_time = true;
  }

  if (in.mutable_attributes.has_mode()) {
    out->mode = in.mutable_attributes.mode();
    out->has.mode = true;
  }

  if (in.mutable_attributes.has_uid()) {
    out->uid = in.mutable_attributes.uid();
    out->has.uid = true;
  }

  if (in.mutable_attributes.has_gid()) {
    out->gid = in.mutable_attributes.gid();
    out->has.gid = true;
  }

  if (in.mutable_attributes.has_rdev()) {
    out->rdev = in.mutable_attributes.rdev();
    out->has.rdev = true;
  }
#endif
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  if (in.immutable_attributes.has_change_time()) {
    out->change_time = in.immutable_attributes.change_time();
    out->has.change_time = true;
  }

  if (in.mutable_attributes.has_casefold()) {
    out->casefold = in.mutable_attributes.casefold();
    out->has.casefold = true;
  }

  if (in.immutable_attributes.has_options()) {
    zxio_verification_options_t out_options{};
    fio::wire::VerificationOptions in_options = in.immutable_attributes.options();
    size_t salt_size = in_options.salt().count();
    if (salt_size > ZXIO_MAX_SALT_SIZE)
      return ZX_ERR_INVALID_ARGS;
    out_options.salt_size = salt_size;
    memcpy(out_options.salt, in_options.salt().data(), salt_size);
    out_options.hash_alg = static_cast<zxio_hash_algorithm_t>(in_options.hash_algorithm());
    out->fsverity_options = out_options;
    out->has.fsverity_options = true;
  }

  if (in.immutable_attributes.has_root_hash()) {
    if (!out->fsverity_root_hash) {
      return ZX_ERR_INVALID_ARGS;  // Caller must provide a pointer to write root hash to.
    }
    memcpy(out->fsverity_root_hash, in.immutable_attributes.root_hash().data(),
           ZXIO_ROOT_HASH_LENGTH);
    out->has.fsverity_root_hash = true;
  }

  if (in.mutable_attributes.has_wrapping_key_id()) {
    memcpy(std::begin(out->wrapping_key_id), in.mutable_attributes.wrapping_key_id().begin(),
           ZXIO_WRAPPING_KEY_ID_LENGTH);
    out->has.wrapping_key_id = true;
  }

  if (in.immutable_attributes.has_verity_enabled()) {
    out->fsverity_enabled = in.immutable_attributes.verity_enabled();
    out->has.fsverity_enabled = true;
  }
#endif

  return ZX_OK;
}
