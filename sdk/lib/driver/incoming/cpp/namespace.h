// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_INCOMING_CPP_NAMESPACE_H_
#define LIB_DRIVER_INCOMING_CPP_NAMESPACE_H_

#include <fidl/fuchsia.io/cpp/markers.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/incoming/cpp/service_validator.h>
#include <lib/fdf/cpp/protocol.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/namespace.h>
#include <lib/fidl_driver/cpp/transport.h>
#include <zircon/availability.h>

// Forward declare to avoid pulling in unneeded generated bindings
namespace fuchsia_component_runner {
class ComponentNamespaceEntry;
namespace wire {
class ComponentNamespaceEntry;
}
}  // namespace fuchsia_component_runner

namespace fdf {

namespace internal {

template <typename T>
static constexpr std::false_type always_false{};

// Returns a client_end to the connection.
template <typename ServiceMember>
zx::result<fdf::ClientEnd<typename ServiceMember::ProtocolType>> DriverTransportConnect(
    fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir, std::string_view instance) {
  static_assert((std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                fidl::internal::DriverTransport>),
                "ServiceMember must use DriverTransport. Double check the FIDL protocol.");

  zx::channel client_token, server_token;
  if (zx_status_t status = zx::channel::create(0, &client_token, &server_token); status != ZX_OK) {
    return zx::error(status);
  }

  if (zx_status_t status =
          fdio_service_connect_at(svc_dir.handle()->get(),
                                  component::MakeServiceMemberPath<ServiceMember>(instance).c_str(),
                                  server_token.release());
      status != ZX_OK) {
    return zx::error(status);
  }

  auto [client_end, server_end] = fdf::Endpoints<typename ServiceMember::ProtocolType>::Create();

  if (zx_status_t status =
          fdf::ProtocolConnect(std::move(client_token), std::move(server_end.TakeHandle()));
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(client_end));
}

// Uses the passed in server_end to make the connection.
template <typename ServiceMember>
zx::result<> DriverTransportConnect(fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir,
                                    fdf::ServerEnd<typename ServiceMember::ProtocolType> server_end,
                                    std::string_view instance) {
  static_assert((std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                fidl::internal::DriverTransport>),
                "ServiceMember must use DriverTransport. Double check the FIDL protocol.");

  zx::channel client_token, server_token;
  if (zx_status_t status = zx::channel::create(0, &client_token, &server_token); status != ZX_OK) {
    return zx::error(status);
  }

  if (zx_status_t status =
          fdio_service_connect_at(svc_dir.handle()->get(),
                                  component::MakeServiceMemberPath<ServiceMember>(instance).c_str(),
                                  server_token.release());
      status != ZX_OK) {
    return zx::error(status);
  }

  if (zx_status_t status =
          fdf::ProtocolConnect(std::move(client_token), std::move(server_end.TakeHandle()));
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok();
}
}  // namespace internal

// Manages a driver's namespace.
class Namespace final {
 public:
  // Creates a namespace from `DriverStartArgs::ns`.
  static zx::result<Namespace> Create(
      fidl::VectorView<fuchsia_component_runner::wire::ComponentNamespaceEntry>& entries);

  // Creates a namespace from natural types version of `DriverStartArgs::ns`.
  static zx::result<Namespace> Create(
      std::vector<fuchsia_component_runner::ComponentNamespaceEntry>& entries);

  Namespace() = default;
  ~Namespace();

  Namespace(Namespace&& other) noexcept;
  Namespace& operator=(Namespace&& other) noexcept;

#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  void SetServiceValidator(std::optional<ServiceValidator> service_validator) {
    service_validator_ = std::move(service_validator);
  }
#endif

  // Connect to a protocol within a driver's namespace.
  // DriverTransport is not supported. Protocols using DriverTransport must be service members.
  template <typename Protocol, typename = std::enable_if_t<!fidl::IsServiceMemberV<Protocol>>>
  zx::result<fidl::ClientEnd<Protocol>> Connect(
      const char* protocol_name = fidl::DiscoverableProtocolName<Protocol>) const {
    static_assert((std::is_same_v<typename Protocol::Transport, fidl::internal::ChannelTransport>),
                  "Protocol must use ChannelTransport. Use a ServiceMember for DriverTransport.");
    return component::ConnectAt<Protocol>(svc_dir(), protocol_name);
  }

  // Connects |server_end| to a protocol within a driver's namespace.
  // DriverTransport is not supported. Protocols using DriverTransport must be service members.
  template <typename Protocol, typename = std::enable_if_t<!fidl::IsServiceMemberV<Protocol>>>
  zx::result<> Connect(fidl::ServerEnd<Protocol> server_end,
                       const char* protocol_name = fidl::DiscoverableProtocolName<Protocol>) const {
    static_assert((std::is_same_v<typename Protocol::Transport, fidl::internal::ChannelTransport>),
                  "Protocol must use ChannelTransport. Use a ServiceMember for DriverTransport.");
    return component::ConnectAt(svc_dir(), std::move(server_end), protocol_name);
  }

  // Connect to a service within a driver's namespace.
  template <typename FidlService>
  zx::result<typename FidlService::ServiceClient> OpenService(std::string_view instance) const {
    static_assert(fidl::IsServiceV<FidlService>, "FidlService must be a service.");
    return component::OpenServiceAt<FidlService>(svc_dir(), instance);
  }

  // Protocol must compose fuchsia.io/Node.
  // DriverTransport is not supported. Protocols using DriverTransport must be service members.
  template <typename Protocol>
  zx::result<fidl::ClientEnd<Protocol>> Open(const char* path, fuchsia_io::Flags flags) const {
    static_assert(!fidl::IsServiceMemberV<Protocol>, "Protocol must not be a ServiceMember.");
    static_assert((std::is_same_v<typename Protocol::Transport, fidl::internal::ChannelTransport>),
                  "Protocol must use ChannelTransport. Use a ServiceMember for DriverTransport.");
    auto [client_end, server_end] = fidl::Endpoints<Protocol>::Create();
    zx::result result = Open(path, flags, server_end.TakeChannel());
    if (result.is_error()) {
      return result.take_error();
    }
    return zx::ok(std::move(client_end));
  }

  // Connects to the |ServiceMember| protocol.
  //
  // |instance| refers to the name of the instance of the service.
  //
  // Returns a ClientEnd of type corresponding to the given protocol
  // e.g. fidl::ClientEnd or fdf::ClientEnd.
  template <typename ServiceMember>
  zx::result<fidl::internal::ClientEndType<typename ServiceMember::ProtocolType>> Connect(
      std::string_view instance = component::kDefaultInstance) const {
    static_assert(
        fidl::IsServiceMemberV<ServiceMember>,
        "ServiceMember type must be the Protocol inside of a Service, eg: fuchsia_hardware_pci::Service::Device.");
    if constexpr (std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                 fidl::internal::ChannelTransport>) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
      if (service_validator_) {
        if (!service_validator_->IsValidZirconServiceInstance(
                std::string(ServiceMember::ServiceName), std::string(instance))) {
          return zx::error(ZX_ERR_NOT_FOUND);
        }
      }
#endif

      return component::ConnectAtMember<ServiceMember>(svc_dir(), instance);
    } else if constexpr (std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                        fidl::internal::DriverTransport>) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
      if (service_validator_) {
        if (!service_validator_->IsValidDriverServiceInstance(
                std::string(ServiceMember::ServiceName), std::string(instance))) {
          return zx::error(ZX_ERR_NOT_FOUND);
        }
      }
#endif
      return internal::DriverTransportConnect<ServiceMember>(svc_dir(), instance);
    } else {
      static_assert(internal::always_false<ServiceMember>);
    }
  }

  // Connects |server_end| to the |ServiceMember| protocol.
  //
  // |instance| refers to the name of the instance of the service.
  //
  // The type of |server_end| must correspond to the given protocol's transport
  // e.g. fidl::ServerEnd for ChannelTransport or fdf::ServerEnd for DriverTransport.
  template <typename ServiceMember>
  zx::result<> Connect(
      fidl::internal::ServerEndType<typename ServiceMember::ProtocolType> server_end,
      std::string_view instance = component::kDefaultInstance) const {
    static_assert(
        fidl::IsServiceMemberV<ServiceMember>,
        "ServiceMember type must be the Protocol inside of a Service, eg: fuchsia_hardware_pci::Service::Device.");
    if constexpr (std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                 fidl::internal::ChannelTransport>) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
      if (service_validator_) {
        if (!service_validator_->IsValidZirconServiceInstance(
                std::string(ServiceMember::ServiceName), std::string(instance))) {
          return zx::error(ZX_ERR_NOT_FOUND);
        }
      }
#endif

      return component::ConnectAtMember<ServiceMember>(svc_dir(), std::move(server_end), instance);
    } else if constexpr (std::is_same_v<typename ServiceMember::ProtocolType::Transport,
                                        fidl::internal::DriverTransport>) {
#if FUCHSIA_API_LEVEL_AT_LEAST(18)
      if (service_validator_) {
        if (!service_validator_->IsValidDriverServiceInstance(
                std::string(ServiceMember::ServiceName), std::string(instance))) {
          return zx::error(ZX_ERR_NOT_FOUND);
        }
      }
#endif
      return internal::DriverTransportConnect<ServiceMember>(svc_dir(), std::move(server_end),
                                                             instance);
    } else {
      static_assert(internal::always_false<ServiceMember>);
    }
  }

  fidl::UnownedClientEnd<fuchsia_io::Directory> svc_dir() const { return svc_dir_; }

 private:
  explicit Namespace(fdio_ns_t* incoming, fidl::ClientEnd<fuchsia_io::Directory> svc_dir);

  Namespace(const Namespace& other) = delete;
  Namespace& operator=(const Namespace& other) = delete;

  // Opens |path| in the driver's namespace. DriverTransport is not supported.
  zx::result<> Open(const char* path, fuchsia_io::Flags flags, zx::channel server_end) const;

  fdio_ns_t* incoming_ = nullptr;
  fidl::ClientEnd<fuchsia_io::Directory> svc_dir_;

#if FUCHSIA_API_LEVEL_AT_LEAST(18)
  std::optional<ServiceValidator> service_validator_;
#endif
};

}  // namespace fdf

#endif  // LIB_DRIVER_INCOMING_CPP_NAMESPACE_H_
