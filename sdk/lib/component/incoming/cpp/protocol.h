// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_COMPONENT_INCOMING_CPP_PROTOCOL_H_
#define LIB_COMPONENT_INCOMING_CPP_PROTOCOL_H_

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/component/incoming/cpp/internal.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>

#include <string_view>
#include <type_traits>
#include <utility>

namespace component {

// Opens the directory containing incoming services in the component's incoming
// namespace.
//
// `path` must be absolute, containing a leading "/". Defaults to "/svc".
//
// # Errors
//
//   * `ZX_ERR_INVALID_ARGS`: `path` is invalid.
//   * `ZX_ERR_NOT_FOUND`: `path` was not found in the incoming namespace.
zx::result<fidl::ClientEnd<fuchsia_io::Directory>> OpenServiceRoot(
    std::string_view path = component::kServiceDirectory);

// Connects to the FIDL Protocol in the component's incoming namespace.
//
// `path` must be absolute, containing a leading "/". Default to "/svc/{name}"
// where `{name}` is the fully qualified name of the FIDL Protocol.
//
// # Errors
//
//   * `ZX_ERR_INVALID_ARGS`: `name` is invalid.
//   * `ZX_ERR_NOT_FOUND`: No entry was found under the given `name` inside "/svc".
template <typename Protocol>
zx::result<fidl::ClientEnd<Protocol>> Connect(
    std::string_view name = fidl::DiscoverableProtocolDefaultPath<Protocol>) {
  auto channel = internal::ConnectRaw(name);
  if (channel.is_error()) {
    return channel.take_error();
  }
  return zx::ok(fidl::ClientEnd<Protocol>(std::move(channel.value())));
}

// Same as above but allows specifying a custom server end for the request.
// This is useful if you'd like to forward requests to another server in
// the namespace.
template <typename Protocol, typename = std::enable_if_t<fidl::IsProtocolV<Protocol>>>
zx::result<> Connect(fidl::ServerEnd<Protocol> server_end,
                     std::string_view path = fidl::DiscoverableProtocolDefaultPath<Protocol>) {
  return internal::ConnectRaw(server_end.TakeChannel(), path);
}

// Connects to the FIDL Protocol relative to the provided `svc_dir` directory.
//
// `name` must be an entry in the `svc_dir` directory. Defaults to the fully
// qualified name fo the FIDL Protocol.
//
// # Errors
//
//   * `ZX_ERR_INVALID_ARGS`: `svc_dir` is an invalid handle.
//   * `ZX_ERR_NOT_FOUND`: No entry was found under the given `name` inside `svc_dir`.
template <typename Protocol, typename = std::enable_if_t<fidl::IsProtocolV<Protocol>>>
zx::result<fidl::ClientEnd<Protocol>> ConnectAt(
    fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir,
    std::string_view name = fidl::DiscoverableProtocolName<Protocol>) {
  auto channel = internal::ConnectAtRaw(svc_dir, name);
  if (channel.is_error()) {
    return channel.take_error();
  }
  return zx::ok(fidl::ClientEnd<Protocol>(std::move(channel.value())));
}

// Same as above but allows specifying a customer server end for the request.
template <typename Protocol, typename = std::enable_if_t<fidl::IsProtocolV<Protocol>>>
zx::result<> ConnectAt(fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir,
                       fidl::ServerEnd<Protocol> server_end,
                       std::string_view name = fidl::DiscoverableProtocolName<Protocol>) {
  if (zx::result<> status = internal::ConnectAtRaw(svc_dir, server_end.TakeChannel(), name);
      status.is_error()) {
    return status.take_error();
  }
  return zx::ok();
}

}  // namespace component

#endif  // LIB_COMPONENT_INCOMING_CPP_PROTOCOL_H_
