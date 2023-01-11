// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/component/incoming/cpp/internal.h>
#include <lib/fdio/directory.h>

namespace component {
namespace internal {

zx::result<zx::channel> ConnectRaw(std::string_view path) {
  zx::channel client_end, server_end;
  if (zx_status_t status = zx::channel::create(0, &client_end, &server_end); status != ZX_OK) {
    return zx::error(status);
  }
  if (zx::result<> status = ConnectRaw(std::move(server_end), path); status.is_error()) {
    return status.take_error();
  }
  return zx::ok(std::move(client_end));
}

zx::result<> ConnectRaw(zx::channel server_end, std::string_view path) {
  if (zx_status_t status = fdio_service_connect(std::string(path).c_str(), server_end.release());
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok();
}

zx::result<zx::channel> ConnectAtRaw(fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir,
                                     std::string_view protocol_name) {
  zx::channel client_end, server_end;
  if (zx_status_t status = zx::channel::create(0, &client_end, &server_end); status != ZX_OK) {
    return zx::error(status);
  }
  if (zx::result<> status = ConnectAtRaw(svc_dir, std::move(server_end), protocol_name);
      status.is_error()) {
    return status.take_error();
  }
  return zx::ok(std::move(client_end));
}

zx::result<> ConnectAtRaw(fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir,
                          zx::channel server_end, std::string_view protocol_name) {
  if (zx_status_t status = fdio_service_connect_at(
          svc_dir.handle()->get(), std::string(protocol_name).c_str(), server_end.release());
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok();
}

zx::result<> CloneRaw(fidl::UnownedClientEnd<fuchsia_io::Node>&& node, zx::channel server_end) {
  const fidl::Status result =
      fidl::WireCall(node)->Clone(fuchsia_io::wire::OpenFlags::kCloneSameRights,
                                  fidl::ServerEnd<fuchsia_io::Node>(std::move(server_end)));
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok();
}

zx::result<> CloneRaw(fidl::UnownedClientEnd<fuchsia_unknown::Cloneable>&& cloneable,
                      zx::channel server_end) {
  const fidl::Status result = fidl::WireCall(cloneable)->Clone2(
      fidl::ServerEnd<fuchsia_unknown::Cloneable>(std::move(server_end)));
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok();
}

zx::result<> DirectoryOpenFunc(zx::unowned_channel dir, fidl::StringView path,
                               fidl::internal::AnyTransport remote) {
  fidl::UnownedClientEnd<fuchsia_io::Directory> dir_end(dir);
  fidl::ServerEnd<fuchsia_io::Node> node_end(remote.release<fidl::internal::ChannelTransport>());
  fidl::Status result = fidl::WireCall<fuchsia_io::Directory>(dir_end)->Open(
      fuchsia_io::wire::OpenFlags::kRightReadable, 0755u, path, std::move(node_end));
  return zx::make_result(result.status());
}

}  // namespace internal

}  // namespace component
