// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_DDKTL_INCLUDE_DDKTL_METADATA_SERVER_H_
#define SRC_LIB_DDKTL_INCLUDE_DDKTL_METADATA_SERVER_H_

#include <fidl/fuchsia.driver.metadata/cpp/fidl.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>

// This header is an adapter for DFv1 drivers to use the //sdk/lib/driver/metadata/cpp library.

namespace ddk {

// When converting a driver from DFv1 to DFv2, replace references to `ddk::ObjectDetails` with the
// `fdf_metadata::ObjectDetails` class found in the //sdk/lib/driver/metadata/cpp library.
//
// This template class must be explicitly specialized with the given |FidlType| in order for
// `ddk::MetadataServer` and `ddk::GetMetadata()` to work. This specialization must define
// `ddk::ObjectDetails::Name`. `ddk::ObjectDetails::Name` is used by `ddk::MetadataServer` and
// `ddk::GetMetadata()` in order to correctly identify the name of the FIDL service used to pass
// metadata. Typically it will be related to the name of |FidlType|.
//
// For example, say there exists a FIDL type `fuchsia.hardware.test/Metadata` that is to be sent
// with `ddk::MetadataServer` and received with `ddk::GetMetadata()`:
//
//   library fuchsia.hardware.test;
//
//   type Metadata = table {
//       1: test_property string:MAX;
//   };
//
// There should be a `ddk::ObjectDetails<fuchsia_hardware_test::Metadata>` class that defines
// `ddk::ObjectDetails::Name`:
//
//   namespace ddk {
//
//     template <>
//     struct ObjectDetails<fuchsia_hardware_test::Metadata> {
//       inline static const char* Name = "fuchsia.hardware.test.Metadata";
//     };
//
//   }  // namespace ddk
template <typename FidlType>
struct ObjectDetails {
  inline static const char* Name;
};

// Returns the path to the fuchsia.driver.metadata/Metadata FIDL service instance for |FidlType|
// within an incoming namespace at instance |instance_name|.
template <typename FidlType>
std::string GetMetadataFidlProtocolPath(std::string_view instance_name) {
  // The metadata protocol is found within the `ObjectDetails<FidlType>::Name` service directory
  // and not the `fuchsia_driver_metadata::Service::Name` directory because that is where
  // `ddk::MetadataServer` is expected to serve the fuchsia.driver.metadata/Metadata protocol.
  return std::string{ObjectDetails<FidlType>::Name}
      .append("/")
      .append(instance_name)
      .append("/")
      .append(fuchsia_driver_metadata::Service::Metadata::Name);
}

// Connects to the FIDL service that provides |FidlType|. This service is found within |device|'s
// incoming namespace at FIDL service instance |instance_name|.
template <typename FidlType>
zx::result<fidl::ClientEnd<fuchsia_driver_metadata::Metadata>> ConnectToMetadataServer(
    zx_device_t* device,
    std::string_view instance_name = component::OutgoingDirectory::kDefaultServiceInstance) {
  static_assert(fidl::IsFidlType<FidlType>::value, "|FidlType| must be a FIDL domain object.");

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_driver_metadata::Metadata>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "Failed to create endpoints: %s", endpoints.status_string());
    return endpoints.take_error();
  }

  zx_status_t status = device_connect_ns_protocol(
      device, GetMetadataFidlProtocolPath<FidlType>(instance_name).c_str(),
      endpoints->server.TakeChannel().release());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to connect to metadata protocol: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok(std::move(endpoints->client));
}

// When converting a driver from DFv1 to DFv2, replace `ddk::GetMetadata()` with the
// `fdf_metadata::GetMetadata()` function found in the //sdk/lib/driver/metadata/cpp library.
//
// Retrieves metadata from the incoming namespace of |device| found at instance |instance_name|.
// The metadata is expected to be served by `ddk::MetadataServer<|FidlType|>`.
// `ddk::ObjectDetails<|FidlType|>::Name` must be defined.
//
// Make sure that the component manifest declares that it uses the
// `ddk::ObjectDetails<|FidlType|>::Name` service.
template <typename FidlType>
zx::result<FidlType> GetMetadata(
    zx_device_t* device,
    std::string_view instance_name = component::OutgoingDirectory::kDefaultServiceInstance) {
  static_assert(fidl::IsFidlType<FidlType>::value, "|FidlType| must be a FIDL domain object.");
  static_assert(!fidl::IsResource<FidlType>::value,
                "|FidlType| cannot be a resource type. Resources cannot be persisted.");

  fidl::WireSyncClient<fuchsia_driver_metadata::Metadata> client{};
  {
    zx::result result = ConnectToMetadataServer<FidlType>(device, instance_name);
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to connect to metadata server: %s", result.status_string());
      return result.take_error();
    }
    client.Bind(std::move(result.value()));
  }

  fidl::WireResult metadata_bytes = client->GetMetadata();
  if (!metadata_bytes.ok()) {
    zxlogf(ERROR, "Failed to send GetMetadata request: %s", metadata_bytes.status_string());
    return zx::error(metadata_bytes.status());
  }
  if (metadata_bytes->is_error()) {
    zxlogf(ERROR, "Failed to get metadata bytes: %s",
           zx_status_get_string(metadata_bytes->error_value()));
    return zx::error(metadata_bytes->error_value());
  }

  fit::result metadata = fidl::Unpersist<FidlType>(metadata_bytes.value()->metadata.get());
  if (metadata.is_error()) {
    zxlogf(ERROR, "Failed to unpersist metadata: %s",
           zx_status_get_string(metadata.error_value().status()));
    return zx::error(metadata.error_value().status());
  }

  return zx::ok(metadata.value());
}

// This function is the same as `ddk::GetMetadata<FidlType>()` except that it will return a
// `std::nullopt` if there is no metadata FIDL protocol within |device|'s incoming namespace at
// |instance_name|.
template <typename FidlType>
zx::result<std::optional<FidlType>> GetMetadataIfExists(
    zx_device_t* device,
    std::string_view instance_name = component::OutgoingDirectory::kDefaultServiceInstance) {
  static_assert(fidl::IsFidlType<FidlType>::value, "|FidlType| must be a FIDL domain object.");
  static_assert(!fidl::IsResource<FidlType>::value,
                "|FidlType| cannot be a resource type. Resources cannot be persisted.");

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_driver_metadata::Metadata>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "Failed to create endpoints: %s", endpoints.status_string());
    return endpoints.take_error();
  }

  zx_status_t status = device_connect_ns_protocol(
      device, GetMetadataFidlProtocolPath<FidlType>(instance_name).c_str(),
      endpoints->server.TakeChannel().release());
  if (status != ZX_OK) {
    zxlogf(DEBUG, "Failed to connect to metadata protocol: %s", zx_status_get_string(status));
    return zx::ok(std::nullopt);
  }

  fidl::WireSyncClient<fuchsia_driver_metadata::Metadata> client{std::move(endpoints->client)};

  fidl::WireResult metadata_bytes = client->GetMetadata();
  if (!metadata_bytes.ok()) {
    zxlogf(DEBUG, "Failed to send GetMetadata request: %s", metadata_bytes.status_string());
    return zx::ok(std::nullopt);
  }
  if (metadata_bytes->is_error()) {
    zxlogf(ERROR, "Failed to get metadata bytes: %s",
           zx_status_get_string(metadata_bytes->error_value()));
    return zx::error(metadata_bytes->error_value());
  }

  fit::result metadata = fidl::Unpersist<FidlType>(metadata_bytes.value()->metadata.get());
  if (metadata.is_error()) {
    zxlogf(ERROR, "Failed to unpersist metadata: %s",
           zx_status_get_string(metadata.error_value().status()));
    return zx::error(metadata.error_value().status());
  }

  return zx::ok(std::optional(metadata.value()));
}

// When converting a driver from DFv1 to DFv2, replace usages of the `ddk::MetadataServer` class
// with the `fdf_metadata::MetadataServer` class found in the //sdk/lib/driver/metadata/cpp library.
//
// Serves metadata that can be retrieved using `ddk::GetMetadata<|FidlType|>()`.
// `ddk::ObjectDetails<|FidlType|>::Name` must be defined. Expected to be used by driver
// components.
//
// As an example, lets say there exists a FIDl type `fuchsia.hardware.test/Metadata` to be sent from
// a driver to its child driver:
//
//   library fuchsia.hardware.test;
//
//   type Metadata = table {
//       1: test_property string:MAX;
//   };
//
// The parent driver can define a `MetadataServer<fuchsia_hardware_test::Metadata>` server
// instance as one its members:
//
//   namespace ddk {
//
//     template <>
//     struct ObjectDetails<fuchsia_hardware_test::Metadata> {
//       inline static const char* Name = "fuchsia.hardware.test.Metadata";
//     };
//
//   }  // namespace ddk
//
//   class ParentDriver : public fdf::DriverBase {
//    private:
//     using MetadataServer = ddk::MetadataServer<fuchsia_hardware_test::Metadata>
//
//     MetadataServer metadata_server_;
//   }
//
// When the parent driver creates a child device, it can offer the metadata server's service to the
// child device by adding the metadata server's offers to the device-add arguments:
//
//   std::array offers = {MetadataServer::kFidlServiceName};
//   ddk::DeviceAddArgs args{"child"};
//   args.set_fidl_service_offers(offers))
//
// The parent driver should also declare the metadata server's capability and offer it in the
// driver's component manifest:
//
//   capabilities: [
//     { service: "fuchsia.hardware.test.Metadata" },
//   ],
//   expose: [
//     {
//       service: "fuchsia.hardware.test.Metadata",
//       from: "self",
//     },
//   ],
//
// See `ddk::ObjectDetails` for more details about the name of the service. See
// //src/lib/ddktl/tests/metadata-server-test for more fleshed out examples.
template <typename FidlType>
class MetadataServer final : public fidl::WireServer<fuchsia_driver_metadata::Metadata> {
 public:
  // Name of the service directory that will serve the fuchsia.driver.metadata/Service FIDL service.
  inline static const char* kFidlServiceName = ObjectDetails<FidlType>::Name;

  explicit MetadataServer(
      std::string instance_name = component::OutgoingDirectory::kDefaultServiceInstance)
      : instance_name_(std::move(instance_name)) {}

  // Set the metadata to be served to |metadata|.
  zx_status_t SetMetadata(const FidlType& metadata) {
    static_assert(fidl::IsFidlType<FidlType>::value, "|FidlType| must be a FIDL domain object.");
    static_assert(!fidl::IsResource<FidlType>::value,
                  "|FidlType| cannot be a resource type. Resources cannot be persisted.");

    fit::result encoded = fidl::Persist(metadata);
    if (encoded.is_error()) {
      zxlogf(ERROR, "Failed to persist metadata: %s", encoded.error_value().status_string());
      return encoded.error_value().status();
    }
    encoded_metadata_.emplace(encoded.value());

    return ZX_OK;
  }

  // Sets the metadata to be served to the metadata found in the incoming namespace of |device|.
  // If the metadata found in |incoming| changes after this function is called then those changes
  // will not be reflected in the metadata to be served. Make sure that the component
  // manifest specifies that is uses the `ddk::ObjectDetails<|FidlType|>::Name` service
  zx_status_t ForwardMetadata(
      zx_device_t* device,
      std::string_view instance_name = component::OutgoingDirectory::kDefaultServiceInstance) {
    fidl::WireSyncClient<fuchsia_driver_metadata::Metadata> client{};
    {
      zx::result result = ConnectToMetadataServer<FidlType>(device, instance_name);
      if (result.is_error()) {
        zxlogf(ERROR, "Failed to connect to metadata server: %s", result.status_string());
        return result.status_value();
      }
      client.Bind(std::move(result.value()));
    }

    fidl::WireResult result = client->GetMetadata();
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send GetMetadata request: %s", result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "Failed to get metadata bytes: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
    cpp20::span<uint8_t> metadata = result.value()->metadata.get();
    std::vector<uint8_t> copy;
    copy.insert(copy.begin(), metadata.begin(), metadata.end());
    encoded_metadata_.emplace(std::move(copy));

    return ZX_OK;
  }

  // Serves the fuchsia.driver.metadata/Service service to |outgoing| under the service name
  // `ddk::MetadataServer::kFidlServiceName` and instance name
  // `ddk::MetadataServer::instance_name_`.
  zx_status_t Serve(fdf::OutgoingDirectory& outgoing, async_dispatcher_t* dispatcher) {
    return Serve(outgoing.component(), dispatcher);
  }

  // Serves the fuchsia.driver.metadata/Service service to |outgoing| under the service name
  // `ddk::MetadataServer::kFidlServiceName` and instance name
  // `ddk::MetadataServer::instance_name_`.
  zx_status_t Serve(component::OutgoingDirectory& outgoing, async_dispatcher_t* dispatcher) {
    fuchsia_driver_metadata::Service::InstanceHandler handler{
        {.metadata = bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure)}};
    zx::result result = outgoing.AddService(std::move(handler), kFidlServiceName, instance_name_);
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to add service: %s", result.status_string());
      return result.status_value();
    }
    return ZX_OK;
  }

 private:
  // fuchsia.driver.metadata/Metadata protocol implementation.
  void GetMetadata(GetMetadataCompleter::Sync& completer) override {
    if (!encoded_metadata_.has_value()) {
      zxlogf(ERROR, "Metadata not set. Set metadata with SetMetadata() or ForwardMetadata().");
      completer.ReplyError(ZX_ERR_BAD_STATE);
      return;
    }
    completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(encoded_metadata_.value()));
  }

  fidl::ServerBindingGroup<fuchsia_driver_metadata::Metadata> bindings_;

  // Encoded metadata that will be served in this instance's fuchsia.driver.metadata/Metadata
  // protocol.
  std::optional<std::vector<uint8_t>> encoded_metadata_;

  // Name of the instance directory that will serve this instance's fuchsia.driver.metadata/Service
  // service.
  std::string instance_name_;
};

}  // namespace ddk

#endif  // SRC_LIB_DDKTL_INCLUDE_DDKTL_METADATA_SERVER_H_
