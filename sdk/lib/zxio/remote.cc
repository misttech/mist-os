// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fcntl.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.pty/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fit/defer.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/channel.h>
#include <lib/zxio/cpp/vector.h>
#include <lib/zxio/null.h>
#include <lib/zxio/ops.h>
#include <lib/zxio/posix_mode.h>
#include <lib/zxio/types.h>
#include <sys/stat.h>
#include <zircon/compiler.h>
#include <zircon/fidl.h>
#include <zircon/syscalls.h>

#include "private.h"

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
  // TODO(https://fxbug.dev/85843): Once overlapping request and response is allowed, reduce
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
      // TODO(fxbug.dev/52930): return ZXIO_NODE_PROTOCOL_CONNECTOR instead.
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
zxio_node_attributes_t ToZxioNodeAttributes(fio::wire::NodeAttributes attr,
                                            ToZxioAbilities to_zxio) {
  zxio_node_attributes_t zxio_attr = {};
  ZXIO_NODE_ATTR_SET(zxio_attr, protocols, ToZxioNodeProtocols(attr.mode));
  ZXIO_NODE_ATTR_SET(zxio_attr, abilities, to_zxio(attr.mode));
  ZXIO_NODE_ATTR_SET(zxio_attr, id, attr.id);
  ZXIO_NODE_ATTR_SET(zxio_attr, content_size, attr.content_size);
  ZXIO_NODE_ATTR_SET(zxio_attr, storage_size, attr.storage_size);
  ZXIO_NODE_ATTR_SET(zxio_attr, link_count, attr.link_count);
  ZXIO_NODE_ATTR_SET(zxio_attr, creation_time, attr.creation_time);
  ZXIO_NODE_ATTR_SET(zxio_attr, modification_time, attr.modification_time);
  return zxio_attr;
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

template <typename Protocol>
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
    zx::result endpoints = fidl::CreateEndpoints<fio::Node>();
    if (endpoints.is_error()) {
      return endpoints.status_value();
    }
    auto [client_end, server_end] = std::move(endpoints.value());
    const fidl::Status result =
        client()->Clone(fio::wire::OpenFlags::kCloneSameRights, std::move(server_end));
    if (!result.ok()) {
      return result.status();
    }
    *out_handle = client_end.TakeChannel().release();
    return ZX_OK;
  }

  zx_status_t Sync();

  zx_status_t AttrGet(zxio_node_attributes_t* out_attr);

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

  zx_status_t Open2(const char* path, size_t path_len, const zxio_open_options_t* options,
                    zxio_node_attributes_t* inout_attr, zxio_storage_t* storage);

  zx_status_t OpenAsync(uint32_t flags, const char* path, size_t path_len, zx_handle_t request);

  zx_status_t Unlink(const char* name, size_t name_len, int flags);

  zx_status_t TokenGet(zx_handle_t* out_token);

  zx_status_t Rename(const char* old_path, size_t old_path_len, zx_handle_t dst_token,
                     const char* new_path, size_t new_path_len);

  zx_status_t Link(const char* src_path, size_t src_path_len, zx_handle_t dst_token,
                   const char* dst_path, size_t dst_path_len);

  zx_status_t DirentIteratorInit(zxio_dirent_iterator_t* iterator);

  zx_status_t DirentIteratorNext(zxio_dirent_iterator_t* iterator, zxio_dirent_t* inout_entry);

  zx_status_t DirentIteratorRewind(zxio_dirent_iterator_t* iterator);

  void DirentIteratorDestroy(zxio_dirent_iterator_t* iterator);

  zx_status_t GetWindowSize(uint32_t* width, uint32_t* height);

  zx_status_t SetWindowSize(uint32_t width, uint32_t height);

  zx_status_t XattrList(void (*callback)(void* context, const uint8_t* name, size_t name_len),
                        void* context);

  zx_status_t XattrGet(const uint8_t* name, size_t name_len, uint8_t* value, size_t value_capacity,
                       size_t* out_value_actual);

  zx_status_t XattrSet(const uint8_t* name, size_t name_len, const uint8_t* value,
                       size_t value_len);

  zx_status_t XattrRemove(const uint8_t* name, size_t name_len);

  const fidl::WireSyncClient<Protocol>& client() const { return client_; }

 private:
  fidl::WireSyncClient<Protocol> client_;
};

class Pty : public Remote<fuchsia_hardware_pty::Device> {
 public:
  Pty(fidl::ClientEnd<fuchsia_hardware_pty::Device> client_end, zx::eventpair event)
      : Remote(std::move(client_end), kOps), event_(std::move(event)) {}

  zx_status_t Close(const bool should_wait) {
    const zx_status_t status = Remote::Close(should_wait);
    this->~Pty();
    return status;
  }

  zx_status_t Clone(zx_handle_t* out_handle) {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_unknown::Cloneable>();
    if (endpoints.is_error()) {
      return endpoints.status_value();
    }
    auto [client_end, server_end] = std::move(endpoints.value());
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

template <typename Protocol>
zx_status_t Remote<Protocol>::Sync() {
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

template <typename Protocol, typename ToZxioAbilities>
zx_status_t AttrGetCommon(const fidl::WireSyncClient<Protocol>& client, ToZxioAbilities to_zxio,
                          zxio_node_attributes_t* out_attr) {
  const fidl::WireResult result = client->GetAttr();
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  if (const zx_status_t status = response.s; status != ZX_OK) {
    return status;
  }
  *out_attr = ToZxioNodeAttributes(response.attributes, to_zxio);
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
zx_status_t Remote<Protocol>::AttrGet(zxio_node_attributes_t* out_attr) {
  return AttrGetCommon(client(), ToZxioAbilitiesForFile(), out_attr);
}

template <typename Protocol>
zx_status_t Remote<Protocol>::AttrSet(const zxio_node_attributes_t* attr) {
  return AttrSetCommon(client(), ToIo1ModePermissionsForFile(), attr);
}

template <typename Protocol>
zx_status_t Remote<Protocol>::AdvisoryLock(advisory_lock_req* req) {
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
  return zxio_do_vector(vector, vector_count, out_actual,
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

template <typename Protocol>
zx_status_t Remote<Protocol>::Readv(const zx_iovec_t* vector, size_t vector_count,
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

template <typename Protocol>
zx_status_t Remote<Protocol>::ReadvAt(zx_off_t offset, const zx_iovec_t* vector,
                                      size_t vector_count, zxio_flags_t flags, size_t* out_actual) {
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

template <typename Protocol>
zx_status_t Remote<Protocol>::Writev(const zx_iovec_t* vector, size_t vector_count,
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

template <typename Protocol>
zx_status_t Remote<Protocol>::WritevAt(zx_off_t offset, const zx_iovec_t* vector,
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

template <typename Protocol>
zx_status_t Remote<Protocol>::Seek(zxio_seek_origin_t start, int64_t offset, size_t* out_offset) {
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

template <typename Protocol>
zx_status_t Remote<Protocol>::Truncate(uint64_t length) {
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

template <typename Protocol>
zx_status_t Remote<Protocol>::FlagsGet(uint32_t* out_flags) {
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

template <typename Protocol>
zx_status_t Remote<Protocol>::FlagsSet(uint32_t flags) {
  const fidl::WireResult result = client()->SetFlags(static_cast<fio::wire::OpenFlags>(flags));
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  return response.s;
}

template <typename Protocol>
zx_status_t Remote<Protocol>::VmoGet(zxio_vmo_flags_t zxio_flags, zx_handle_t* out_vmo) {
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

template <typename Protocol>
zx_status_t Remote<Protocol>::Open(uint32_t flags, const char* path, size_t path_len,
                                   zxio_storage_t* storage) {
  zx::result endpoints = fidl::CreateEndpoints<fio::Node>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }
  auto [client_end, server_end] = std::move(endpoints.value());
  const fidl::Status result =
      client()->Open(static_cast<fio::wire::OpenFlags>(flags) | fio::wire::OpenFlags::kDescribe, {},
                     fidl::StringView::FromExternal(path, path_len), std::move(server_end));
  if (!result.ok()) {
    return result.status();
  }
  return zxio_create_with_on_open(client_end.TakeChannel().release(), storage);
}

template <typename Protocol>
zx_status_t Remote<Protocol>::Open2(const char* path, size_t path_len,
                                    const zxio_open_options_t* options,
                                    zxio_node_attributes_t* inout_attr, zxio_storage_t* storage) {
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  zx::channel client_end, server_end;
  if (zx_status_t status = zx::channel::create(0, &client_end, &server_end); status != ZX_OK) {
    return status;
  }

  fio::wire::ConnectorFlags connector_flags;
  fio::Operations optional_rights;
  fidl::WireTableFrame<fio::wire::DirectoryProtocolOptions> directory_options_frame;
  fio::wire::DirectoryProtocolOptions directory_options;
  fio::wire::FileProtocolFlags file_flags;
  fio::wire::SymlinkProtocolFlags symlink_flags;
  fidl::WireTableFrame<fio::wire::NodeProtocols> node_protocols_frame;
  fio::wire::NodeProtocols node_protocols;
  fio::Operations rights(options->rights);
  fio::NodeAttributesQuery attributes;
  fidl::WireTableFrame<fio::wire::MutableNodeAttributes> create_attributes_frame;
  fio::wire::MutableNodeAttributes create_attributes;
  fidl::WireTableFrame<fio::wire::NodeOptions> node_options_frame;
  fio::wire::NodeOptions node_options;
  fio::wire::ConnectionProtocols protocols;

  if (options->protocols & ZXIO_NODE_PROTOCOL_CONNECTOR) {
    if (options->protocols != ZXIO_NODE_PROTOCOL_CONNECTOR) {
      return ZX_ERR_INVALID_ARGS;
    }
    protocols = fio::wire::ConnectionProtocols::WithConnector(
        fidl::ObjectView<fio::wire::ConnectorFlags>::FromExternal(&connector_flags));
  } else {
    auto node_options_builder =
        fio::wire::NodeOptions::ExternalBuilder(
            fidl::ObjectView<fidl::WireTableFrame<fio::wire::NodeOptions>>::FromExternal(
                &node_options_frame))
            .flags(fidl::ObjectView<fio::wire::NodeFlags>::FromExternal(
                const_cast<fio::wire::NodeFlags*>(&fio::wire::NodeFlags::kGetRepresentation)));

    // -- protocols --
    auto node_protocols_builder = fio::wire::NodeProtocols::ExternalBuilder(
        fidl::ObjectView<fidl::WireTableFrame<fio::wire::NodeProtocols>>::FromExternal(
            &node_protocols_frame));

    if (options->protocols & ZXIO_NODE_PROTOCOL_DIRECTORY) {
      auto directory_options_builder = fio::wire::DirectoryProtocolOptions::ExternalBuilder(
          fidl::ObjectView<fidl::WireTableFrame<fio::wire::DirectoryProtocolOptions>>::FromExternal(
              &directory_options_frame));
      if (options->optional_rights != 0) {
        optional_rights = fio::Operations(options->optional_rights);
        directory_options_builder.optional_rights(
            fidl::ObjectView<fio::wire::Operations>::FromExternal(&optional_rights));
      }
      directory_options = directory_options_builder.Build();

      node_protocols_builder.directory(
          fidl::ObjectView<fio::wire::DirectoryProtocolOptions>::FromExternal(&directory_options));
    }
    if (options->protocols & ZXIO_NODE_PROTOCOL_FILE) {
      file_flags = fio::FileProtocolFlags(options->file_flags);
      node_protocols_builder.file(
          fidl::ObjectView<fio::wire::FileProtocolFlags>::FromExternal(&file_flags));
    }
    if (options->protocols & ZXIO_NODE_PROTOCOL_SYMLINK) {
      node_protocols_builder.symlink(
          fidl::ObjectView<fio::wire::SymlinkProtocolFlags>::FromExternal(&symlink_flags));
    }
    node_protocols = node_protocols_builder.Build();
    node_options_builder.protocols(
        fidl::ObjectView<fio::wire::NodeProtocols>::FromExternal(&node_protocols));

    // -- mode --
    node_options_builder.mode(fio::wire::OpenMode(options->mode));

    // -- rights --
    if (rights != fio::Operations(0)) {
      node_options_builder.rights(fidl::ObjectView<fio::wire::Operations>::FromExternal(&rights));
    }

    // -- attributes --
    if (inout_attr) {
      if (inout_attr->has.protocols)
        attributes |= fio::NodeAttributesQuery::kProtocols;
      if (inout_attr->has.abilities)
        attributes |= fio::NodeAttributesQuery::kAbilities;
      if (inout_attr->has.content_size)
        attributes |= fio::NodeAttributesQuery::kContentSize;
      if (inout_attr->has.storage_size)
        attributes |= fio::NodeAttributesQuery::kStorageSize;
      if (inout_attr->has.link_count)
        attributes |= fio::NodeAttributesQuery::kLinkCount;
      if (inout_attr->has.id)
        attributes |= fio::NodeAttributesQuery::kId;
      if (inout_attr->has.creation_time)
        attributes |= fio::NodeAttributesQuery::kCreationTime;
      if (inout_attr->has.modification_time)
        attributes |= fio::NodeAttributesQuery::kModificationTime;
      if (inout_attr->has.mode)
        attributes |= fio::NodeAttributesQuery::kMode;
      if (inout_attr->has.uid)
        attributes |= fio::NodeAttributesQuery::kUid;
      if (inout_attr->has.gid)
        attributes |= fio::NodeAttributesQuery::kGid;
      if (inout_attr->has.rdev)
        attributes |= fio::NodeAttributesQuery::kRdev;

      if (attributes) {
        node_options_builder.attributes(
            fidl::ObjectView<fio::wire::NodeAttributesQuery>::FromExternal(&attributes));
      } else {
        inout_attr = nullptr;
      }
    }

    // -- create_attributes --
    if (options->create_attr) {
      auto builder = fio::wire::MutableNodeAttributes::ExternalBuilder(
          fidl::ObjectView<fidl::WireTableFrame<fio::wire::MutableNodeAttributes>>::FromExternal(
              &create_attributes_frame));

      if (options->create_attr->has.protocols || options->create_attr->has.abilities ||
          options->create_attr->has.id || options->create_attr->has.content_size ||
          options->create_attr->has.storage_size || options->create_attr->has.link_count) {
        return ZX_ERR_INVALID_ARGS;
      }

      if (options->create_attr->has.creation_time) {
        builder.creation_time(fidl::ObjectView<uint64_t>::FromExternal(
            const_cast<uint64_t*>(&options->create_attr->creation_time)));
      }
      if (options->create_attr->has.modification_time) {
        builder.modification_time(fidl::ObjectView<uint64_t>::FromExternal(
            const_cast<uint64_t*>(&options->create_attr->modification_time)));
      }
      if (options->create_attr->has.mode) {
        builder.mode(options->create_attr->mode);
      }
      if (options->create_attr->has.uid) {
        builder.uid(options->create_attr->uid);
      }
      if (options->create_attr->has.gid) {
        builder.gid(options->create_attr->gid);
      }
      if (options->create_attr->has.rdev) {
        builder.rdev(fidl::ObjectView<uint64_t>::FromExternal(
            const_cast<uint64_t*>(&options->create_attr->rdev)));
      }

      create_attributes = builder.Build();
      node_options_builder.create_attributes(
          fidl::ObjectView<fio::wire::MutableNodeAttributes>::FromExternal(&create_attributes));
    }

    node_options = node_options_builder.Build();
    protocols = fio::wire::ConnectionProtocols::WithNode(
        fidl::ObjectView<fio::wire::NodeOptions>::FromExternal(&node_options));
  }

  const fidl::Status result = client()->Open2(fidl::StringView::FromExternal(path, path_len),
                                              protocols, std::move(server_end));
  if (!result.ok()) {
    return result.status();
  }
  return zxio_create_with_on_representation(client_end.release(), inout_attr, storage);
#else
  return ZX_ERR_NOT_SUPPORTED;
#endif
}

template <typename Protocol>
zx_status_t Remote<Protocol>::OpenAsync(uint32_t flags, const char* path, size_t path_len,
                                        zx_handle_t request) {
  fidl::ServerEnd<fio::Node> node_request{zx::channel(request)};
  const fidl::Status result =
      client()->Open(static_cast<fio::wire::OpenFlags>(flags), {},
                     fidl::StringView::FromExternal(path, path_len), std::move(node_request));
  return result.status();
}

template <typename Protocol>
zx_status_t Remote<Protocol>::Unlink(const char* name, size_t name_len, int flags) {
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

template <typename Protocol>
zx_status_t Remote<Protocol>::TokenGet(zx_handle_t* out_token) {
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

template <typename Protocol>
zx_status_t Remote<Protocol>::Rename(const char* old_path, size_t old_path_len,
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

template <typename Protocol>
zx_status_t Remote<Protocol>::Link(const char* src_path, size_t src_path_len, zx_handle_t dst_token,
                                   const char* dst_path, size_t dst_path_len) {
  const fidl::WireResult result =
      client()->Link(fidl::StringView::FromExternal(src_path, src_path_len), zx::handle(dst_token),
                     fidl::StringView::FromExternal(dst_path, dst_path_len));
  if (!result.ok()) {
    return result.status();
  }
  const auto& response = result.value();
  return response.s;
}

template <typename Protocol>
zx_status_t Remote<Protocol>::DirentIteratorInit(zxio_dirent_iterator_t* iterator) {
  new (iterator) DirentIteratorImpl(io());
  return ZX_OK;
}

template <typename Protocol>
zx_status_t Remote<Protocol>::DirentIteratorNext(zxio_dirent_iterator_t* iterator,
                                                 zxio_dirent_t* inout_entry) {
  return reinterpret_cast<DirentIteratorImpl*>(iterator)->Next(inout_entry);
}

template <typename Protocol>
zx_status_t Remote<Protocol>::DirentIteratorRewind(zxio_dirent_iterator_t* iterator) {
  return reinterpret_cast<DirentIteratorImpl*>(iterator)->Rewind();
}

template <typename Protocol>
void Remote<Protocol>::DirentIteratorDestroy(zxio_dirent_iterator_t* iterator) {
  reinterpret_cast<DirentIteratorImpl*>(iterator)->~DirentIteratorImpl();
}

zx_status_t Pty::IsAtty(bool* tty) {
  *tty = true;
  return ZX_OK;
}

template <typename Protocol>
zx_status_t Remote<Protocol>::GetWindowSize(uint32_t* width, uint32_t* height) {
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

template <typename Protocol>
zx_status_t Remote<Protocol>::SetWindowSize(uint32_t width, uint32_t height) {
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

template <typename Protocol>
zx_status_t Remote<Protocol>::XattrList(void (*callback)(void* context, const uint8_t* name,
                                                         size_t name_len),
                                        void* context) {
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  if (!client().is_valid()) {
    return ZX_ERR_BAD_STATE;
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::ExtendedAttributeIterator>();
  if (endpoints.is_error()) {
    return endpoints.error_value();
  }
  auto [client_end, server_end] = std::move(endpoints.value());
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
#endif
}

template <typename Protocol>
zx_status_t Remote<Protocol>::XattrGet(const uint8_t* name, size_t name_len, uint8_t* value,
                                       size_t value_capacity, size_t* out_value_actual) {
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
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
  if (value_resp->is_bytes()) {
    const fidl::VectorView value_bytes = value_resp->bytes();
    // Always return the length. If the buffer is too small, it will provide a hint for the caller.
    *out_value_actual = value_bytes.count();
    if (value_bytes.count() > value_capacity) {
      return ZX_ERR_BUFFER_TOO_SMALL;
    }

    memcpy(value, value_bytes.data(), value_bytes.count());
  } else {
    const zx::vmo& value_vmo = value_resp->buffer();
    uint64_t value_size;
    if (zx_status_t status = value_vmo.get_prop_content_size(&value_size); status != ZX_OK) {
      return status;
    }
    // Always return the length. If the buffer is too small, it will provide a hint for the caller.
    *out_value_actual = value_size;
    if (value_size > value_capacity) {
      return ZX_ERR_BUFFER_TOO_SMALL;
    }

    if (zx_status_t status = value_vmo.read(value, 0, value_size); status != ZX_OK) {
      return status;
    }
  }

  return ZX_OK;
#else
  return ZX_ERR_NOT_SUPPORTED;
#endif
}

template <typename Protocol>
zx_status_t Remote<Protocol>::XattrSet(const uint8_t* name, size_t name_len, const uint8_t* value,
                                       size_t value_len) {
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
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

  const fidl::WireResult result = client()->SetExtendedAttribute(
      fidl::VectorView<uint8_t>(arena, cpp20::span(name, name_len)), std::move(attribute_value));
  if (!result.ok()) {
    return result.status();
  }
  if (result.value().is_error()) {
    return result.value().error_value();
  }

  return ZX_OK;
#else
  return ZX_ERR_NOT_SUPPORTED;
#endif
}

template <typename Protocol>
zx_status_t Remote<Protocol>::XattrRemove(const uint8_t* name, size_t name_len) {
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
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
#endif
}

class Node : public Remote<fio::Node> {
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

class Directory : public Remote<fio::Directory> {
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

    return zxio_do_vector(vector, vector_count, out_actual,
                          [](void* buffer, size_t capacity, size_t* out_actual) {
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

  zx_status_t AttrGet(zxio_node_attributes_t* out_attr) {
    return AttrGetCommon(client(), ToZxioAbilitiesForDirectory(), out_attr);
  }

  zx_status_t AttrSet(const zxio_node_attributes_t* attr) {
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
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
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
#endif
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
  ops.open2 = Adaptor::From<&Directory::Open2>;
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

class File : public Remote<fio::File> {
 public:
  File(fidl::ClientEnd<fio::File> client_end, zx::event event, zx::stream stream)
      : Remote(std::move(client_end), kOps), event_(std::move(event)), stream_(std::move(stream)) {}

 private:
  zx_status_t Close(const bool should_wait) {
    const zx_status_t status = Remote::Close(should_wait);
    this->~File();
    return status;
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
  return ops;
})();

#if __Fuchsia_API_level__ >= FUCHSIA_HEAD

class Symlink : public Remote<fio::Symlink> {
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
  return ops;
})();

#endif  // #if __Fuchsia_API_level__ >= FUCHSIA_HEAD

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

#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
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
