// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/start_args.h>

#include <utility>

namespace fdf_testing {

zx::result<CreateStartArgsResult> CreateStartArgs(fdf_testing::TestNode& node_server) {
  zx::result incoming_directory_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (incoming_directory_endpoints.is_error()) {
    return incoming_directory_endpoints.take_error();
  }

  zx::result outgoing_directory_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (outgoing_directory_endpoints.is_error()) {
    return outgoing_directory_endpoints.take_error();
  }

  zx::result incoming_node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (incoming_node_endpoints.is_error()) {
    return incoming_node_endpoints.take_error();
  }

  // NodeServer::Serve is thread-unsafe.
  async::PostTask(
      node_server.dispatcher(),
      [&node_server, incoming_node_server = std::move(incoming_node_endpoints->server)]() mutable {
        auto serve_result = node_server.Serve(std::move(incoming_node_server));
        ZX_ASSERT_MSG(serve_result.is_ok(), "Failed to serve fdf::Node protocol: %s",
                      serve_result.status_string());
      });

  auto incoming_entries = std::vector<fuchsia_component_runner::ComponentNamespaceEntry>(1);
  incoming_entries[0] = fuchsia_component_runner::ComponentNamespaceEntry({
      .path = "/",
      .directory = std::move(incoming_directory_endpoints->client),
  });

  auto start_args = fdf::DriverStartArgs({
      .node = std::move(incoming_node_endpoints->client),
      .incoming = std::move(incoming_entries),
      .outgoing_dir = std::move(outgoing_directory_endpoints->server),
  });

  return zx::ok(CreateStartArgsResult{
      .start_args = std::move(start_args),
      .incoming_directory_server = std::move(incoming_directory_endpoints->server),
      .outgoing_directory_client = std::move(outgoing_directory_endpoints->client),
  });
}

}  // namespace fdf_testing
