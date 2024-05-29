// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_METADATA_CPP_METADATA_SERVER_H_
#define LIB_DRIVER_METADATA_CPP_METADATA_SERVER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.driver.metadata/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/driver/metadata/cpp/metadata.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>

namespace fdf_metadata {

// Serves metadata that can be retrieved using `fdf_metadata::GetMetadata<|FidlType|>()`.
// `fdf_metadata::ObjectDetails<|FidlType|>::Name` must be defined. Expected to be used by driver
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
//   namespace fdf_metadata {
//
//     template <>
//     struct ObjectDetails<fuchsia_hardware_test::Metadata> {
//       inline static const char* Name = "fuchsia.hardware.test.Metadata";
//     };
//
//   }  // namespace fdf_metadata
//
//   class ParentDriver : public fdf::DriverBase {
//    private:
//     fdf_metadata::MetadataServer<fuchsia_hardware_test::Metadata> metadata_server_;
//   }
//
// When the parent driver creates a child node, it can offer the metadata server's service to the
// child node by adding the metadata server's offers to the node-add arguments:
//
//   auto args = fuchsia_driver_framework::NodeAddArgs args{{.offers2 =
//     std::vector{metadata_server_.MakeOffer()}}};
//
// The parent driver should also declare the metadata server's capability and offer it in the
// driver's component manifest like so:
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
// See `fdf_metadata::ObjectDetails` for more details about the name of the service.
template <typename FidlType, typename = std::enable_if_t<fidl::IsFidlObject<FidlType>::value>>
class MetadataServer final : public fidl::WireServer<fuchsia_driver_metadata::Metadata> {
 public:
  // Make sure that the component manifest specifies `MetadataServer::kServiceName` as a service
  // capability and exposes it.
  explicit MetadataServer(
      std::string instance_name = component::OutgoingDirectory::kDefaultServiceInstance)
      : instance_name_(std::move(instance_name)) {}

  // Set the metadata to be served to |metadata|. |metadata| must be persistable.
  zx_status_t SetMetadata(const FidlType& metadata) {
    fit::result data = fidl::Persist(metadata);
    if (data.is_error()) {
      FDF_SLOG(ERROR, "Failed to persist metadata.",
               KV("status", data.error_value().status_string()));
      return data.error_value().status();
    }
    metadata_.emplace(data.value());

    return ZX_OK;
  }

  // Sets the metadata to be served to the metadata found in |incoming|.
  // If the metadata found in |incoming| changes after this function is called then those changes
  // will not be reflected in the metadata to be served. Make sure that the component
  // manifest specifies that is uses the `fdf_metadata::ObjectDetails<|FidlType|>::Name` service
  zx_status_t ForwardMetadata(
      const std::shared_ptr<fdf::Namespace>& incoming,
      std::string_view instance_name = component::OutgoingDirectory::kDefaultServiceInstance) {
    zx::result metadata = fdf_metadata::GetMetadata<FidlType>(incoming, instance_name);
    if (metadata.is_error()) {
      FDF_SLOG(ERROR, "Failed to get metadata.", KV("status", metadata.error_value()));
      return metadata.status_value();
    }

    zx_status_t status = SetMetadata(metadata.value());
    if (status != ZX_OK) {
      FDF_SLOG(ERROR, "Failed to set metadata.", KV("status", zx_status_get_string(status)));
      return status;
    }

    return ZX_OK;
  }

  zx_status_t Serve(fdf::OutgoingDirectory& outgoing, async_dispatcher_t* dispatcher) {
    return Serve(outgoing.component(), dispatcher);
  }

  // Serves the fuchsia.driver.metadata/Service service to |outgoing| under the service name
  // `MetadataServer::kServiceName` and instance name `MetadataServer::instance_name_`.
  zx_status_t Serve(component::OutgoingDirectory& outgoing, async_dispatcher_t* dispatcher) {
    fuchsia_driver_metadata::Service::InstanceHandler handler{
        {.metadata = bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure)}};
    zx::result result = outgoing.AddService(std::move(handler), kServiceName, instance_name_);
    if (result.is_error()) {
      FDF_SLOG(ERROR, "Failed to add service.", KV("status", result.status_string()));
      return result.status_value();
    }
    return ZX_OK;
  }

  // Creates an offer for this `MetadataServer` instance's fuchsia.driver.metadata/Service
  // service.
  fuchsia_driver_framework::Offer MakeOffer() {
    return fuchsia_driver_framework::Offer::WithZirconTransport(
        fdf::MakeOffer(kServiceName, instance_name_));
  }

  fuchsia_driver_framework::wire::Offer MakeOffer(fidl::AnyArena& arena) {
    return fuchsia_driver_framework::wire::Offer::WithZirconTransport(
        arena, fdf::MakeOffer(arena, kServiceName, instance_name_));
  }

 private:
  // Name of the service directory that will serve the fuchsia.driver.metadata/Service service.
  inline static const std::string_view kServiceName = ObjectDetails<FidlType>::Name;

  // fuchsia.driver.metadata/Metadata protocol implementation.
  void GetMetadata(GetMetadataCompleter::Sync& completer) override {
    if (!metadata_.has_value()) {
      FDF_LOG(ERROR, "Metadata not set.");
      completer.ReplyError(ZX_ERR_BAD_STATE);
      return;
    }
    completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(metadata_.value()));
  }

  fidl::ServerBindingGroup<fuchsia_driver_metadata::Metadata> bindings_;

  // Serialized version of the metadata that will be served in this instance's
  // fuchsia.driver.metadata/Metadata protocol.
  std::optional<std::vector<uint8_t>> metadata_;

  // Name of the instance directory that will serve this instance's fuchsia.driver.metadata/Service
  // service.
  std::string instance_name_;
};

}  // namespace fdf_metadata

#endif  // LIB_DRIVER_METADATA_CPP_METADATA_SERVER_H_
