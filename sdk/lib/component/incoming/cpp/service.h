// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_COMPONENT_INCOMING_CPP_SERVICE_H_
#define LIB_COMPONENT_INCOMING_CPP_SERVICE_H_

#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.unknown/cpp/wire.h>
#include <lib/component/incoming/cpp/internal.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>

#include <type_traits>
#include <utility>

namespace component {

// Gets the relative path to a service member.
//
// This is in the format of:
// `ServiceName/instance/Name`
//
// This relative path can be appended to `/svc/` to construct a full path.
template <typename ServiceMember,
          typename = std::enable_if_t<fidl::IsServiceMemberV<ServiceMember>>>
std::string MakeServiceMemberPath(std::string_view instance) {
  return std::string(ServiceMember::ServiceName)
      .append("/")
      .append(instance)
      .append("/")
      .append(ServiceMember::Name);
}

// Typed channel wrapper around |fdio_service_connect_at|.
//
// Connects to the |Protocol| protocol relative to the |svc_dir| directory.
// |protocol_name| defaults to the fully qualified name of the FIDL protocol,
// but may be overridden to a custom value.
//
// See `ConnectAt(UnownedClientEnd<fuchsia_io::Directory>, std::string_view)` for
// details.
template <typename ServiceMember,
          typename = std::enable_if_t<fidl::IsServiceMemberV<ServiceMember>>>
zx::result<fidl::ClientEnd<typename ServiceMember::ProtocolType>> ConnectAt(
    fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir,
    std::string_view instance = kDefaultInstance) {
  auto path = MakeServiceMemberPath<ServiceMember>(instance);
  auto channel = internal::ConnectAtRaw(svc_dir, path.c_str());
  if (channel.is_error()) {
    return channel.take_error();
  }
  return zx::ok(fidl::ClientEnd<typename ServiceMember::ProtocolType>(std::move(channel.value())));
}

// Typed channel wrapper around |fdio_service_connect_at|.
//
// Connects |server_end| to the |Protocol| protocol relative to the |svc_dir|
// directory. |protocol_name| defaults to the fully qualified name of the FIDL
// protocol, but may be overridden to a custom value.
//
// See documentation on |fdio_service_connect_at| for details.
template <typename ServiceMember,
          typename = std::enable_if_t<fidl::IsServiceMemberV<ServiceMember>>>
zx::result<> ConnectAt(fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir,
                       fidl::ServerEnd<typename ServiceMember::ProtocolType> server_end,
                       std::string_view instance = kDefaultInstance) {
  auto path = MakeServiceMemberPath<ServiceMember>(instance);
  if (zx::result<> status = internal::ConnectAtRaw(svc_dir, server_end.TakeChannel(), path.c_str());
      status.is_error()) {
    return status.take_error();
  }
  return zx::ok();
}

// Opens a connection to the default instance of a FIDL service of type `FidlService`, rooted at
// `dir`. The default instance is called 'default'. See
// `OpenServiceAt(zx::unowned_channel,fidl::StringView)` for details.
template <typename FidlService>
zx::result<typename FidlService::ServiceClient> OpenServiceAt(
    fidl::UnownedClientEnd<fuchsia_io::Directory> dir);

// Opens a connection to the given instance of a FIDL service of type `FidlService`, rooted at
// `dir`. The result, if successful, is a `FidlService::ServiceClient` that exposes methods that
// connect to the various members of the FIDL service.
//
// If the service or instance does not exist, the resulting `FidlService::ServiceClient` will
// fail to connect to a member.
//
// Returns a zx::result of status Ok on success. In the event of failure, an error status
// variant is returned, set to an error value.
//
// Returns a zx::result of state Error set to ZX_ERR_INVALID_ARGS if `instance` is more than 255
// characters long.
//
// ## Example
//
// ```C++
// using Echo = fuchsia_echo::Echo;
// using EchoService = fuchsia_echo::EchoService;
//
// zx::result<EchoService::ServiceClient> open_result =
//     sys::OpenServiceAt<EchoService>(std::move(svc_));
// ASSERT_TRUE(open_result.is_ok());
//
// EchoService::ServiceClient service = open_result.take_value();
//
// zx::result<fidl::ClientEnd<Echo>> connect_result = service.ConnectFoo();
// ASSERT_TRUE(connect_result.is_ok());
//
// fidl::WireSyncClient<Echo> client{connect_result.take_value()};
// ```
template <typename FidlService>
zx::result<typename FidlService::ServiceClient> OpenServiceAt(
    fidl::UnownedClientEnd<fuchsia_io::Directory> dir, std::string_view instance);

// Opens a connection to the given instance of a FIDL service with the name `service_name`,
// rooted
//    at `dir`. The `remote` channel is passed to the remote service, and its local twin can be
// used
// to
// issue FIDL protocol messages. Most callers will want to use `OpenServiceAt(...)`.
//
// If the service or instance does not exist, the `remote` channel will be closed.
//
// Returns ZX_OK on success. In the event of failure, an error value is returned.
//
// Returns ZX_ERR_INVALID_ARGS if `service_path` or `instance` are more than 255
// characters long.
zx::result<> OpenNamedServiceAt(fidl::UnownedClientEnd<fuchsia_io::Directory> dir,
                                std::string_view service_path, std::string_view instance,
                                zx::channel remote);

template <typename FidlService>
zx::result<typename FidlService::ServiceClient> OpenServiceAt(
    fidl::UnownedClientEnd<fuchsia_io::Directory> dir, std::string_view instance) {
  zx::channel local, remote;
  if (zx_status_t status = zx::channel::create(0, &local, &remote); status != ZX_OK) {
    return zx::error(status);
  }

  zx::result<> result = OpenNamedServiceAt(dir, FidlService::Name, instance, std::move(remote));
  if (result.is_error()) {
    return result.take_error();
  }
  return zx::ok(typename FidlService::ServiceClient(std::move(local), internal::DirectoryOpenFunc));
}

template <typename FidlService>
zx::result<typename FidlService::ServiceClient> OpenServiceAt(
    fidl::UnownedClientEnd<fuchsia_io::Directory> dir) {
  return OpenServiceAt<FidlService>(dir, kDefaultInstance);
}
}  // namespace component

#endif  // LIB_COMPONENT_INCOMING_CPP_SERVICE_H_
