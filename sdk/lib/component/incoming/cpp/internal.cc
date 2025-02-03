// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/component/incoming/cpp/constants.h>
#include <lib/component/incoming/cpp/internal.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>
#include <zircon/assert.h>

namespace component::internal {

namespace {

namespace fio = fuchsia_io;

constexpr uint64_t kMaxFilename = fio::wire::kMaxFilename;

// Max path length will be two path components, separated by a file separator.
constexpr uint64_t kMaxPath = (2 * kMaxFilename) + 1;

zx::result<fidl::StringView> ValidateAndJoinPath(fidl::Array<char, kMaxPath>& buffer,
                                                 fidl::StringView service,
                                                 fidl::StringView instance) {
  if (service.empty() || service.size() > kMaxFilename) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (instance.size() > kMaxFilename) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (service[0] == '/') {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const uint64_t path_size = service.size() + instance.size() + 1;
  ZX_ASSERT(path_size <= kMaxPath);

  char* path_cursor = buffer.data();
  memcpy(path_cursor, service.data(), service.size());
  path_cursor += service.size();
  *path_cursor++ = '/';
  memcpy(path_cursor, instance.data(), instance.size());
  return zx::ok(fidl::StringView::FromExternal(buffer.data(), path_size));
}

zx::result<fio::wire::Flags> EnsureDirectoryProtocol(fio::wire::Flags flags) {
  const fio::wire::Flags protocols = flags & fio::wire::kMaskKnownProtocols;
  if (protocols && (protocols != fio::wire::Flags::kProtocolDirectory)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  return zx::ok(flags | fio::wire::Flags::kProtocolDirectory);
}

}  // namespace

zx::result<> ConnectRaw(zx::channel server_end, std::string_view path) {
  std::string owned_path(path);
  if (zx_status_t status = fdio_service_connect(owned_path.c_str(), server_end.release());
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok();
}

zx::result<> ConnectAtRaw(fidl::UnownedClientEnd<fio::Directory> svc_dir, zx::channel server_end,
                          std::string_view protocol_name) {
  std::string path(protocol_name);
  if (zx_status_t status =
          fdio_service_connect_at(svc_dir.handle()->get(), path.c_str(), server_end.release());
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok();
}

zx::result<fidl::ClientEnd<fio::Directory>> OpenDirectory(std::string_view path,
                                                          fio::wire::Flags flags) {
  zx::result directory_flags = EnsureDirectoryProtocol(flags);
  if (directory_flags.is_error()) {
    return directory_flags.take_error();
  }
  auto [client, server] = fidl::Endpoints<fio::Directory>::Create();
  std::string owned_path(path);
  if (zx_status_t status = fdio_open3(owned_path.c_str(), uint64_t{*directory_flags},
                                      server.TakeChannel().release());
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(client));
}

zx::result<fidl::ClientEnd<fio::Directory>> OpenDirectoryAt(
    fidl::UnownedClientEnd<fio::Directory> dir, std::string_view path, fio::wire::Flags flags) {
  zx::result directory_flags = EnsureDirectoryProtocol(flags);
  if (directory_flags.is_error()) {
    return directory_flags.take_error();
  }
  auto [client, server] = fidl::Endpoints<fio::Directory>::Create();
  std::string owned_path(path);
  if (zx_status_t status =
          fdio_open3_at(dir.handle()->get(), owned_path.c_str(), uint64_t{*directory_flags},
                        server.TakeChannel().release());
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(client));
}

zx::result<> CloneRaw(fidl::UnownedClientEnd<fuchsia_unknown::Cloneable>&& cloneable,
                      zx::channel server_end) {
#if FUCHSIA_API_LEVEL_AT_LEAST(26)
  const fidl::Status result = fidl::WireCall(cloneable)->Clone(
      fidl::ServerEnd<fuchsia_unknown::Cloneable>(std::move(server_end)));
#else
  const fidl::Status result = fidl::WireCall(cloneable)->Clone2(
      fidl::ServerEnd<fuchsia_unknown::Cloneable>(std::move(server_end)));
#endif
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok();
}

zx::result<> OpenNamedServiceRaw(std::string_view service, std::string_view instance,
                                 zx::channel remote) {
  auto client = OpenServiceRoot();
  if (client.is_error()) {
    return client.take_error();
  }
  return OpenNamedServiceAtRaw(*client, service, instance, std::move(remote));
}

zx::result<> OpenNamedServiceAtRaw(fidl::UnownedClientEnd<fio::Directory> dir,
                                   std::string_view service, std::string_view instance,
                                   zx::channel remote) {
  fidl::Array<char, kMaxPath> path_buffer;
  zx::result<fidl::StringView> path_result =
      ValidateAndJoinPath(path_buffer, fidl::StringView::FromExternal(service),
                          fidl::StringView::FromExternal(instance));
  if (!path_result.is_ok()) {
    return path_result.take_error();
  }
  return DirectoryOpenFunc(dir.channel(), path_result.value(),
                           fidl::internal::MakeAnyTransport(std::move(remote)));
}

zx::result<> DirectoryOpenFunc(zx::unowned_channel dir, fidl::StringView path,
                               fidl::internal::AnyTransport remote) {
  std::string owned_path(path.get());
  return zx::make_result(
      fdio_service_connect_at(dir->get(), owned_path.c_str(),
                              remote.release<fidl::internal::ChannelTransport>().release()));
}

}  // namespace component::internal
