// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/metadata/cpp/metadata.h>

namespace fdf_metadata {
zx::result<fidl::ClientEnd<fuchsia_driver_metadata::Metadata>> ConnectToMetadataProtocol(
    const fdf::Namespace& incoming, std::string_view service_name, std::string_view instance_name) {
  // The metadata protocol is found within the `service_name` service directory and not the
  // `fuchsia_driver_metadata::Service::Name` directory because that is where
  // `fdf_metadata::MetadataServer` is expected to serve the fuchsia.driver.metadata/Metadata'
  // protocol.
  auto path = std::string{service_name}
                  .append("/")
                  .append(instance_name)
                  .append("/")
                  .append(fuchsia_driver_metadata::Service::Metadata::Name);

  zx::result result =
      component::ConnectAt<fuchsia_driver_metadata::Metadata>(incoming.svc_dir(), path);
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to connect to metadata protocol.", KV("status", result.status_string()),
             KV("path", path));
    return result.take_error();
  }

  return zx::ok(std::move(result.value()));
}
}  // namespace fdf_metadata
