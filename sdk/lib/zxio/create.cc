// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.pty/cpp/wire.h>
#include <fidl/fuchsia.posix.socket/cpp/wire.h>
#include <fidl/fuchsia.unknown/cpp/wire.h>
#include <lib/fit/defer.h>
#include <lib/zx/handle.h>
#include <lib/zx/vmo.h>
#include <lib/zxio/cpp/inception.h>
#include <lib/zxio/null.h>
#include <lib/zxio/types.h>
#include <lib/zxio/zxio.h>
#include <stdarg.h>
#include <zircon/availability.h>
#include <zircon/errors.h>

#include "private.h"

namespace fio = fuchsia_io;
namespace funknown = fuchsia_unknown;

namespace {

// A zxio_handle_holder is a zxio object instance that holds on to a handle and
// allows it to be closed or released via zxio_close() / zxio_release(). It is
// useful for wrapping objects that zxio does not understand.
struct zxio_handle_holder {
  zxio_t io;
  zx::handle handle;
};

static_assert(sizeof(zxio_handle_holder) <= sizeof(zxio_storage_t),
              "zxio_handle_holder must fit inside zxio_storage_t.");

zxio_handle_holder& zxio_get_handle_holder(zxio_t* io) {
  return *reinterpret_cast<zxio_handle_holder*>(io);
}

constexpr zxio_ops_t zxio_handle_holder_ops = []() {
  zxio_ops_t ops = zxio_default_ops;
  ops.close = [](zxio_t* io, const bool should_wait) {
    zxio_get_handle_holder(io).~zxio_handle_holder();
    return ZX_OK;
  };

  ops.release = [](zxio_t* io, zx_handle_t* out_handle) {
    const zx_handle_t handle = zxio_get_handle_holder(io).handle.release();
    if (handle == ZX_HANDLE_INVALID) {
      return ZX_ERR_BAD_HANDLE;
    }
    *out_handle = handle;
    return ZX_OK;
  };
  return ops;
}();

void zxio_handle_holder_init(zxio_storage_t* storage, zx::handle handle) {
  auto holder = new (storage) zxio_handle_holder{
      .handle = std::move(handle),
  };
  zxio_init(&holder->io, &zxio_handle_holder_ops);
}

class ZxioCreateOnOpenEventHandler final : public fidl::WireSyncEventHandler<fio::Node> {
 public:
  ZxioCreateOnOpenEventHandler(fidl::ClientEnd<fio::Node> node, zxio_storage_t* storage,
                               zx_status_t& status)
      : node_(std::move(node)), storage_(storage), status_(status) {}

 protected:
  void OnOpen(fidl::WireEvent<fio::Node::OnOpen>* event) final {
    status_ = [&event = *event, this] {
      if (event.s != ZX_OK) {
        return event.s;
      }
      if (!event.info.has_value()) {
        return ZX_ERR_INVALID_ARGS;
      }
      return zxio_create_with_nodeinfo_deprecated(std::move(node_), event.info.value(), storage_);
    }();
  }

  void OnRepresentation(fidl::WireEvent<fio::Node::OnRepresentation>* event) final {
    status_ = ZX_ERR_NOT_SUPPORTED;
  }

  void handle_unknown_event(fidl::UnknownEventMetadata<fio::Node> metadata) final {}

 private:
  fidl::ClientEnd<fio::Node> node_;
  zxio_storage_t* storage_;
  zx_status_t& status_;
};

class ZxioCreateOnRepresentationEventHandler final : public fidl::WireSyncEventHandler<fio::Node> {
 public:
  ZxioCreateOnRepresentationEventHandler(fidl::ClientEnd<fio::Node> node,
                                         zxio_node_attributes_t* attr, zxio_storage_t* storage,
                                         zx_status_t& status)
      : node_(std::move(node)), attr_(attr), storage_(storage), status_(status) {}

 protected:
  void OnOpen(fidl::WireEvent<fio::Node::OnOpen>* event) final { status_ = ZX_ERR_NOT_SUPPORTED; }

  void OnRepresentation(fidl::WireEvent<fio::Node::OnRepresentation>* event) final {
    status_ = zxio_create_with_representation(std::move(node_), *event, attr_, storage_);
  }

  void handle_unknown_event(fidl::UnknownEventMetadata<fio::Node> metadata) final {}

 private:
  fidl::ClientEnd<fio::Node> node_;
  zxio_node_attributes_t* attr_;
  zxio_storage_t* storage_;
  zx_status_t& status_;
};

// Same as `fidl::DiscoverableProtocolName` except using a `std::string_view`.
template <typename Protocol>
constexpr std::string_view kProtocolName = fidl::DiscoverableProtocolName<Protocol>;

// TODO(https://fxbug.dev/42056856): We can't mark certain fuchsia.io protocols as @discoverable
// so we need to hard-code these constants until they are updated.
template <>
constexpr std::string_view kProtocolName<fio::Directory> = "fuchsia.io.Directory";
template <>
constexpr std::string_view kProtocolName<fio::File> = "fuchsia.io.File";

// Maps a given discoverable FIDL protocol to an equivalent ZXIO type. If the protocol is unknown,
// it will be mapped to `ZXIO_OBJECT_TYPE_NONE`.
constexpr zxio_object_type_t ProtocolToObjectType(std::string_view protocol) {
  // Note the static assertions for each protocol to object type mapping. If the FIDL @discoverable
  // protocol name ever changes, care needs to be taken to ensure backwards compatibility with
  // existing servers until they are updated.
  static_assert(kProtocolName<fio::Directory> == "fuchsia.io.Directory");
  if (protocol == kProtocolName<fio::Directory>) {
    return ZXIO_OBJECT_TYPE_DIR;
  }
  static_assert(kProtocolName<fio::File> == "fuchsia.io.File");
  if (protocol == kProtocolName<fio::File>) {
    return ZXIO_OBJECT_TYPE_FILE;
  }
  static_assert(kProtocolName<fuchsia_hardware_pty::Device> == "fuchsia.hardware.pty.Device");
  if (protocol == kProtocolName<fuchsia_hardware_pty::Device>) {
    return ZXIO_OBJECT_TYPE_TTY;
  }
  static_assert(kProtocolName<fuchsia_posix_socket::DatagramSocket> ==
                "fuchsia.posix.socket.DatagramSocket");
  if (protocol == kProtocolName<fuchsia_posix_socket::DatagramSocket>) {
    return ZXIO_OBJECT_TYPE_DATAGRAM_SOCKET;
  }
  static_assert(kProtocolName<fuchsia_posix_socket::StreamSocket> ==
                "fuchsia.posix.socket.StreamSocket");
  if (protocol == kProtocolName<fuchsia_posix_socket::StreamSocket>) {
    return ZXIO_OBJECT_TYPE_STREAM_SOCKET;
  }
  static_assert(kProtocolName<fuchsia_posix_socket::SynchronousDatagramSocket> ==
                "fuchsia.posix.socket.SynchronousDatagramSocket");
  if (protocol == kProtocolName<fuchsia_posix_socket::SynchronousDatagramSocket>) {
    return ZXIO_OBJECT_TYPE_SYNCHRONOUS_DATAGRAM_SOCKET;
  }
  static_assert(kProtocolName<fuchsia_posix_socket_packet::Socket> ==
                "fuchsia.posix.socket.packet.Socket");
  if (protocol == kProtocolName<fuchsia_posix_socket_packet::Socket>) {
    return ZXIO_OBJECT_TYPE_PACKET_SOCKET;
  }
  static_assert(kProtocolName<fuchsia_posix_socket_raw::Socket> ==
                "fuchsia.posix.socket.raw.Socket");
  if (protocol == kProtocolName<fuchsia_posix_socket_raw::Socket>) {
    return ZXIO_OBJECT_TYPE_RAW_SOCKET;
  }
  return ZXIO_OBJECT_TYPE_NONE;
}

}  // namespace

zx::result<zxio_object_type_t> zxio_get_object_type(
    const fidl::ClientEnd<fuchsia_unknown::Queryable>& queryable) {
  const fidl::WireResult result = fidl::WireCall(queryable)->Query();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  const fidl::WireResponse response = result.value();
  const std::string_view protocol{reinterpret_cast<const char*>(response.protocol.data()),
                                  response.protocol.count()};
  const zxio_object_type_t type = ProtocolToObjectType(protocol);
  if (type != ZXIO_OBJECT_TYPE_NONE) {
    return zx::ok(type);
  }
  // TODO(https://fxbug.dev/42056856): We can remove the checks for legacy protocol name constants
  // once all prebuilt servers are using the latest SDK.
  if (protocol == std::string_view{fio::wire::kFileProtocolName}) {
    return zx::ok(ZXIO_OBJECT_TYPE_FILE);
  }
  if (protocol == std::string_view{fio::wire::kDirectoryProtocolName}) {
    return zx::ok(ZXIO_OBJECT_TYPE_DIR);
  }
  if (protocol == std::string_view{fuchsia_hardware_pty::wire::kDeviceProtocolName}) {
    return zx::ok(ZXIO_OBJECT_TYPE_TTY);
  }
  if (protocol == std::string_view{fuchsia_posix_socket::wire::kDatagramSocketProtocolName}) {
    return zx::ok(ZXIO_OBJECT_TYPE_DATAGRAM_SOCKET);
  }
  if (protocol == std::string_view{fuchsia_posix_socket::wire::kStreamSocketProtocolName}) {
    return zx::ok(ZXIO_OBJECT_TYPE_STREAM_SOCKET);
  }
  if (protocol ==
      std::string_view{fuchsia_posix_socket::wire::kSynchronousDatagramSocketProtocolName}) {
    return zx::ok(ZXIO_OBJECT_TYPE_SYNCHRONOUS_DATAGRAM_SOCKET);
  }
  if (protocol == std::string_view{fuchsia_posix_socket_packet::wire::kSocketProtocolName}) {
    return zx::ok(ZXIO_OBJECT_TYPE_PACKET_SOCKET);
  }
  if (protocol == std::string_view{fuchsia_posix_socket_raw::wire::kSocketProtocolName}) {
    return zx::ok(ZXIO_OBJECT_TYPE_RAW_SOCKET);
  }
  return zx::ok(ZXIO_OBJECT_TYPE_NONE);
}

zx_status_t zxio_create_with_info(zx_handle_t raw_handle, const zx_info_handle_basic_t* handle_info,
                                  zxio_storage_t* storage) {
  zx::handle handle(raw_handle);
  if (!handle.is_valid() || storage == nullptr || handle_info == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }
  switch (handle_info->type) {
    case ZX_OBJ_TYPE_CHANNEL: {
      fidl::ClientEnd<funknown::Queryable> queryable(zx::channel(std::move(handle)));
      zx::result type = zxio_get_object_type(queryable);
      if (type.is_error()) {
        return type.error_value();
      }
      fidl::Arena alloc;
      switch (type.value()) {
        case ZXIO_OBJECT_TYPE_FILE: {
          const fidl::UnownedClientEnd<fio::File> file(queryable.borrow().channel());
          fidl::WireResult result = fidl::WireCall(file)->Describe();
          if (!result.ok()) {
            return result.status();
          }

          auto file_info = fidl::ObjectView<fio::wire::FileInfo>::FromExternal(&result.value());
          auto representation = fio::wire::Representation::WithFile(file_info);

          return zxio_create_with_representation(
              fidl::ClientEnd<fio::Node>{queryable.TakeChannel()}, representation, nullptr,
              storage);
        }
        case ZXIO_OBJECT_TYPE_DIR: {
          auto representation = fio::wire::Representation::WithDirectory({});
          return zxio_create_with_representation(
              fidl::ClientEnd<fio::Node>{queryable.TakeChannel()}, representation, nullptr,
              storage);
        }
        case ZXIO_OBJECT_TYPE_TTY: {
          const fidl::UnownedClientEnd<fuchsia_hardware_pty::Device> device(
              queryable.borrow().channel());
          fidl::WireResult result = fidl::WireCall(device)->Describe();
          if (!result.ok()) {
            return result.status();
          }
          fidl::WireResponse response = result.value();
          return zxio_pty_init(
              storage, response.has_event() ? std::move(response.event()) : zx::eventpair{},
              fidl::ClientEnd<fuchsia_hardware_pty::Device>(queryable.TakeChannel()));
        }
        case ZXIO_OBJECT_TYPE_DATAGRAM_SOCKET: {
          const fidl::UnownedClientEnd<fuchsia_posix_socket::DatagramSocket> socket(
              queryable.borrow().channel());
          fidl::WireResult result = fidl::WireCall(socket)->Describe();
          if (!result.ok()) {
            return result.status();
          }
          fidl::WireResponse response = result.value();
          if (!(response.has_socket() && response.has_tx_meta_buf_size() &&
                response.has_rx_meta_buf_size() &&
                response.has_metadata_encoding_protocol_version())) {
            return ZX_ERR_INVALID_ARGS;
          }
          zx_info_socket_t info;
          if (const zx_status_t status =
                  response.socket().get_info(ZX_INFO_SOCKET, &info, sizeof(info), nullptr, nullptr);
              status != ZX_OK) {
            return status;
          }
          const zxio_datagram_prelude_size_t prelude_size{
              .tx = response.tx_meta_buf_size(),
              .rx = response.rx_meta_buf_size(),
          };
          return zxio_datagram_socket_init(
              storage, std::move(response.socket()), info, prelude_size,
              fidl::ClientEnd<fuchsia_posix_socket::DatagramSocket>(queryable.TakeChannel()));
        }
        case ZXIO_OBJECT_TYPE_STREAM_SOCKET: {
          const fidl::UnownedClientEnd<fuchsia_posix_socket::StreamSocket> socket(
              queryable.borrow().channel());
          fidl::WireResult result = fidl::WireCall(socket)->Describe();
          if (!result.ok()) {
            return result.status();
          }
          fidl::WireResponse response = result.value();
          if (!response.has_socket()) {
            return ZX_ERR_INVALID_ARGS;
          }
          bool is_connected;
          const zx_status_t status =
              // TODO(https://fxbug.dev/42068636): define this and other signals in FIDL.
              response.socket().wait_one(ZX_USER_SIGNAL_3, zx::time::infinite_past(), nullptr);
          // TODO(https://fxbug.dev/42161904): Transferring a listening or connecting socket to
          // another process doesn't work correctly since those states can't be observed here.
          switch (status) {
            case ZX_OK:
              is_connected = true;
              break;
            case ZX_ERR_TIMED_OUT:
              is_connected = false;
              break;
            default:
              return status;
          }
          zx_info_socket_t info;
          if (const zx_status_t status =
                  response.socket().get_info(ZX_INFO_SOCKET, &info, sizeof(info), nullptr, nullptr);
              status != ZX_OK) {
            return status;
          }
          return zxio_stream_socket_init(
              storage, std::move(response.socket()), info, is_connected,
              fidl::ClientEnd<fuchsia_posix_socket::StreamSocket>(queryable.TakeChannel()));
        }
        case ZXIO_OBJECT_TYPE_SYNCHRONOUS_DATAGRAM_SOCKET: {
          const fidl::UnownedClientEnd<fuchsia_posix_socket::SynchronousDatagramSocket> socket(
              queryable.borrow().channel());
          fidl::WireResult result = fidl::WireCall(socket)->Describe();
          if (!result.ok()) {
            return result.status();
          }
          fidl::WireResponse response = result.value();
          if (!response.has_event()) {
            return ZX_ERR_INVALID_ARGS;
          }
          return zxio_synchronous_datagram_socket_init(
              storage, std::move(response.event()),
              fidl::ClientEnd<fuchsia_posix_socket::SynchronousDatagramSocket>(
                  queryable.TakeChannel()));
        }
        case ZXIO_OBJECT_TYPE_PACKET_SOCKET: {
          const fidl::UnownedClientEnd<fuchsia_posix_socket_packet::Socket> socket(
              queryable.borrow().channel());
          fidl::WireResult result = fidl::WireCall(socket)->Describe();
          if (!result.ok()) {
            return result.status();
          }
          fidl::WireResponse response = result.value();
          if (!response.has_event()) {
            return ZX_ERR_INVALID_ARGS;
          }
          return zxio_packet_socket_init(
              storage, std::move(response.event()),
              fidl::ClientEnd<fuchsia_posix_socket_packet::Socket>(queryable.TakeChannel()));
        }
        case ZXIO_OBJECT_TYPE_RAW_SOCKET: {
          const fidl::UnownedClientEnd<fuchsia_posix_socket_raw::Socket> socket(
              queryable.borrow().channel());
          fidl::WireResult result = fidl::WireCall(socket)->Describe();
          if (!result.ok()) {
            return result.status();
          }
          fidl::WireResponse response = result.value();
          if (!response.has_event()) {
            return ZX_ERR_INVALID_ARGS;
          }
          return zxio_raw_socket_init(
              storage, std::move(response.event()),
              fidl::ClientEnd<fuchsia_posix_socket_raw::Socket>(queryable.TakeChannel()));
        }
        case ZXIO_OBJECT_TYPE_NONE: {
          auto representation = fio::wire::Representation::WithConnector({});
          return zxio_create_with_representation(
              fidl::ClientEnd<fio::Node>{queryable.TakeChannel()}, representation, nullptr,
              storage);
        }
        default: {
          return ZX_ERR_INVALID_ARGS;
        }
      }
    }
    case ZX_OBJ_TYPE_LOG: {
      zxio_debuglog_init(storage, zx::debuglog(std::move(handle)));
      return ZX_OK;
    }
    case ZX_OBJ_TYPE_SOCKET: {
      zx::socket socket(std::move(handle));
      zx_info_socket_t info;
      const zx_status_t status =
          socket.get_info(ZX_INFO_SOCKET, &info, sizeof(info), nullptr, nullptr);
      if (status != ZX_OK) {
        return status;
      }
      return zxio_pipe_init(storage, std::move(socket), info);
    }
    case ZX_OBJ_TYPE_VMO: {
      zx::vmo vmo(std::move(handle));
      zx::stream stream;
      uint32_t options = 0u;
      if (handle_info->rights & ZX_RIGHT_READ) {
        options |= ZX_STREAM_MODE_READ;
      }
      if (handle_info->rights & ZX_RIGHT_WRITE) {
        options |= ZX_STREAM_MODE_WRITE;
      }
      // We pass 0 for the initial seek value because the |handle| we're given does not remember
      // the seek value we had previously.
      const zx_status_t status = zx::stream::create(options, vmo, 0u, &stream);
      if (status != ZX_OK) {
        auto vmo_is_phys_or_contig = [&]() {
          zx_info_vmo_t vmo_info;
          const zx_status_t info_status =
              vmo.get_info(ZX_INFO_VMO, &vmo_info, sizeof(vmo_info), nullptr, nullptr);
          if (info_status == ZX_OK) {
            if (!(vmo_info.flags & ZX_INFO_VMO_TYPE_PAGED) ||
                (vmo_info.flags & ZX_INFO_VMO_CONTIGUOUS)) {
              return true;
            }
          }
          return false;
        };
        // Streams are not supported for physical or contiguous VMOs. In this case we will still
        // succeed creation, but read/write operations will not work.
        if (status != ZX_ERR_WRONG_TYPE || !vmo_is_phys_or_contig()) {
          zxio_default_init(&storage->io);
          return status;
        }
      }
      return zxio_vmo_init(storage, std::move(vmo), std::move(stream));
    }
    default: {
      zxio_handle_holder_init(storage, std::move(handle));
      return ZX_ERR_NOT_SUPPORTED;
    }
  }
}

zx_status_t zxio_create(zx_handle_t raw_handle, zxio_storage_t* storage) {
  zx::handle handle(raw_handle);
  if (!handle.is_valid() || storage == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }
  zx_info_handle_basic_t info = {};
  const zx_status_t status =
      handle.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  if (status != ZX_OK) {
    zxio_default_init(&storage->io);
    return status;
  }
  return zxio_create_with_info(handle.release(), &info, storage);
}

zx_status_t zxio_create_with_on_open(zx_handle_t raw_handle, zxio_storage_t* storage) {
  fidl::ClientEnd<fio::Node> node{zx::channel(raw_handle)};
  if (!node.is_valid() || storage == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }
  const fidl::UnownedClientEnd unowned_node = node.borrow();
  zx_status_t handler_status;
  ZxioCreateOnOpenEventHandler handler(std::move(node), storage, handler_status);
  const fidl::Status status = handler.HandleOneEvent(unowned_node);
  if (!status.ok()) {
    if (status.reason() == fidl::Reason::kUnexpectedMessage) {
      return ZX_ERR_IO;
    }
    return status.status();
  }
  return handler_status;
}

zx_status_t zxio_create_with_on_representation(zx_handle_t raw_handle,
                                               zxio_node_attributes_t* inout_attr,
                                               zxio_storage_t* storage) {
  fidl::ClientEnd<fio::Node> node{zx::channel(raw_handle)};
  if (!node.is_valid() || storage == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }
  const fidl::UnownedClientEnd unowned_node = node.borrow();
  zx_status_t handler_status;
  ZxioCreateOnRepresentationEventHandler handler(std::move(node), inout_attr, storage,
                                                 handler_status);
  const fidl::Status status = handler.HandleOneEvent(unowned_node);
  if (!status.ok()) {
    if (status.reason() == fidl::Reason::kUnexpectedMessage) {
      return ZX_ERR_IO;
    }
    return status.status();
  }
  return handler_status;
}

zx_status_t zxio_create_with_nodeinfo_deprecated(fidl::ClientEnd<fio::Node> node,
                                                 fio::wire::NodeInfoDeprecated& info,
                                                 zxio_storage_t* storage) {
  switch (info.Which()) {
    case fio::wire::NodeInfoDeprecated::Tag::kDirectory: {
      return zxio_dir_init(storage, fidl::ClientEnd<fio::Directory>(node.TakeChannel()));
    }
    case fio::wire::NodeInfoDeprecated::Tag::kFile: {
      fio::wire::FileObject& file = info.file();
      zx::event event = std::move(file.event);
      zx::stream stream = std::move(file.stream);
      return zxio_file_init(storage, std::move(event), std::move(stream),
                            fidl::ClientEnd<fio::File>(node.TakeChannel()));
    }
    case fio::wire::NodeInfoDeprecated::Tag::kService: {
      return zxio_node_init(storage, std::move(node));
    }
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
    case fio::wire::NodeInfoDeprecated::Tag::kSymlink: {
      fio::wire::SymlinkObject& symlink = info.symlink();
      const auto& span = symlink.target.get();
      return zxio_symlink_init(storage, fidl::ClientEnd<fio::Symlink>(node.TakeChannel()),
                               std::vector(span.begin(), span.end()));
    }
#endif
  }
}

zx_status_t zxio_create_with_representation(fidl::ClientEnd<fio::Node> node,
                                            fio::wire::Representation& representation,
                                            zxio_node_attributes_t* attr, zxio_storage_t* storage) {
  switch (representation.Which()) {
    case fio::wire::Representation::Tag::kConnector: {
      if (attr) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
        fio::wire::ConnectorInfo& connector = representation.connector();
        if (!connector.has_attributes())
          return ZX_ERR_INVALID_ARGS;
        if (zx_status_t status = zxio_attr_from_wire(connector.attributes(), attr); status != ZX_OK)
          return status;
#else
        return ZX_ERR_NOT_SUPPORTED;
#endif
      }
      return zxio_node_init(storage, std::move(node));
    }
    case fio::wire::Representation::Tag::kDirectory: {
      if (attr) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
        fio::wire::DirectoryInfo& dir = representation.directory();
        if (!dir.has_attributes())
          return ZX_ERR_INVALID_ARGS;
        if (zx_status_t status = zxio_attr_from_wire(dir.attributes(), attr); status != ZX_OK)
          return status;
#else
        return ZX_ERR_NOT_SUPPORTED;
#endif
      }
      return zxio_dir_init(storage, fidl::ClientEnd<fio::Directory>(node.TakeChannel()));
    }
    case fio::wire::Representation::Tag::kFile: {
      fio::wire::FileInfo& file = representation.file();
      if (attr) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
        if (!file.has_attributes())
          return ZX_ERR_INVALID_ARGS;
        if (zx_status_t status = zxio_attr_from_wire(file.attributes(), attr); status != ZX_OK)
          return status;
#else
        return ZX_ERR_NOT_SUPPORTED;
#endif
      }
      zx::event event = file.has_observer() ? std::move(file.observer()) : zx::event();
      zx::stream stream = file.has_stream() ? std::move(file.stream()) : zx::stream();
      return zxio_file_init(storage, std::move(event), std::move(stream),
                            fidl::ClientEnd<fio::File>(node.TakeChannel()));
    }
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
    case fio::wire::Representation::Tag::kSymlink: {
      fio::wire::SymlinkInfo& symlink = representation.symlink();
      if (!symlink.has_target())
        return ZX_ERR_INVALID_ARGS;
      if (attr) {
        if (!symlink.has_attributes())
          return ZX_ERR_INVALID_ARGS;
        if (zx_status_t status = zxio_attr_from_wire(symlink.attributes(), attr); status != ZX_OK)
          return status;
      }
      const auto& span = symlink.target();
      return zxio_symlink_init(storage, fidl::ClientEnd<fio::Symlink>(node.TakeChannel()),
                               std::vector(span.begin(), span.end()));
    }
#endif
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

zx_status_t zxio_create_with_type(zxio_storage_t* storage, zxio_object_type_t type, ...) {
  va_list args;
  va_start(args, type);
  const fit::deferred_action va_cleanup = fit::defer([&args]() { va_end(args); });
  switch (type) {
    case ZXIO_OBJECT_TYPE_SYNCHRONOUS_DATAGRAM_SOCKET: {
      zx::eventpair event(va_arg(args, zx_handle_t));
      zx::channel client(va_arg(args, zx_handle_t));
      if (storage == nullptr || !event.is_valid() || !client.is_valid()) {
        return ZX_ERR_INVALID_ARGS;
      }
      return zxio_synchronous_datagram_socket_init(
          storage, std::move(event),
          fidl::ClientEnd<fuchsia_posix_socket::SynchronousDatagramSocket>(std::move(client)));
    }
    case ZXIO_OBJECT_TYPE_DATAGRAM_SOCKET: {
      zx::socket socket(va_arg(args, zx_handle_t));
      zx_info_socket_t* info = va_arg(args, zx_info_socket_t*);
      zxio_datagram_prelude_size_t* prelude_size = va_arg(args, zxio_datagram_prelude_size_t*);
      zx::channel client(va_arg(args, zx_handle_t));
      if (storage == nullptr || !socket.is_valid() || info == nullptr || prelude_size == nullptr ||
          !client.is_valid()) {
        return ZX_ERR_INVALID_ARGS;
      }
      return zxio_datagram_socket_init(
          storage, std::move(socket), *info, *prelude_size,
          fidl::ClientEnd<fuchsia_posix_socket::DatagramSocket>(std::move(client)));
    }
    case ZXIO_OBJECT_TYPE_DIR: {
      zx::channel control(va_arg(args, zx_handle_t));
      if (storage == nullptr || !control.is_valid()) {
        return ZX_ERR_INVALID_ARGS;
      }
      return zxio_dir_init(storage, fidl::ClientEnd<fio::Directory>{std::move(control)});
    }
    case ZXIO_OBJECT_TYPE_NODE: {
      zx::channel control(va_arg(args, zx_handle_t));
      if (storage == nullptr || !control.is_valid()) {
        return ZX_ERR_INVALID_ARGS;
      }
      return zxio_node_init(storage, fidl::ClientEnd<fio::Node>{std::move(control)});
    }
    case ZXIO_OBJECT_TYPE_STREAM_SOCKET: {
      zx::socket socket(va_arg(args, zx_handle_t));
      zx_info_socket_t* info = va_arg(args, zx_info_socket_t*);
      const bool is_connected = va_arg(args, int);
      zx::channel client(va_arg(args, zx_handle_t));
      if (storage == nullptr || !socket.is_valid() || info == nullptr || !client.is_valid()) {
        return ZX_ERR_INVALID_ARGS;
      }
      return zxio_stream_socket_init(
          storage, std::move(socket), *info, is_connected,
          fidl::ClientEnd<fuchsia_posix_socket::StreamSocket>(std::move(client)));
    }
    case ZXIO_OBJECT_TYPE_PIPE: {
      zx::socket socket(va_arg(args, zx_handle_t));
      zx_info_socket_t* info = va_arg(args, zx_info_socket_t*);
      if (storage == nullptr || !socket.is_valid() || info == nullptr) {
        return ZX_ERR_INVALID_ARGS;
      }
      return zxio_pipe_init(storage, std::move(socket), *info);
    }
    case ZXIO_OBJECT_TYPE_RAW_SOCKET: {
      zx::eventpair event(va_arg(args, zx_handle_t));
      zx::channel client(va_arg(args, zx_handle_t));
      if (storage == nullptr || !event.is_valid() || !client.is_valid()) {
        return ZX_ERR_INVALID_ARGS;
      }
      return zxio_raw_socket_init(
          storage, std::move(event),
          fidl::ClientEnd<fuchsia_posix_socket_raw::Socket>(std::move(client)));
    }
    case ZXIO_OBJECT_TYPE_PACKET_SOCKET: {
      zx::eventpair event(va_arg(args, zx_handle_t));
      zx::channel client(va_arg(args, zx_handle_t));
      if (storage == nullptr || !event.is_valid() || !client.is_valid()) {
        return ZX_ERR_INVALID_ARGS;
      }
      return zxio_packet_socket_init(
          storage, std::move(event),
          fidl::ClientEnd<fuchsia_posix_socket_packet::Socket>(std::move(client)));
    }
    case ZXIO_OBJECT_TYPE_VMO: {
      zx::vmo vmo(va_arg(args, zx_handle_t));
      zx::stream stream(va_arg(args, zx_handle_t));
      if (storage == nullptr || !vmo.is_valid() || !stream.is_valid()) {
        return ZX_ERR_INVALID_ARGS;
      }
      return zxio_vmo_init(storage, std::move(vmo), std::move(stream));
    }
    case ZXIO_OBJECT_TYPE_TRANSFERABLE: {
      zx::channel channel(va_arg(args, zx_handle_t));
      if (storage == nullptr || !channel.is_valid()) {
        return ZX_ERR_INVALID_ARGS;
      }
      return zxio_transferable_init(storage, std::move(channel));
    }
    default:
      break;
  }
  return ZX_ERR_NOT_SUPPORTED;
}
