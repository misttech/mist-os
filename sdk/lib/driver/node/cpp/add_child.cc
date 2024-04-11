// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#if __Fuchsia_API_level__ >= 18

#include <lib/driver/node/cpp/add_child.h>

namespace fdf {

zx::result<OwnedChildNode> AddOwnedChild(
    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent, fdf::Logger& logger,
    std::string_view node_name) {
  auto [node_controller_client_end, node_controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  auto [node_client_end, node_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::Node>::Create();

  fuchsia_driver_framework::NodeAddArgs args{{
      .name = {std::string(node_name)},
  }};

  fidl::Result<fuchsia_driver_framework::Node::AddChild> result = fidl::Call(parent)->AddChild(
      {std::move(args), std::move(node_controller_server_end), std::move(node_server_end)});

  if (result.is_error()) {
    FDF_LOGL(ERROR, logger, "Failed to add owned child %s. Error: %s",
             std::string(node_name).c_str(), result.error_value().FormatDescription().c_str());
    return zx::error(result.error_value().is_framework_error()
                         ? result.error_value().framework_error().status()
                         : ZX_ERR_INTERNAL);
  }

  return zx::ok(OwnedChildNode{std::move(node_controller_client_end), std::move(node_client_end)});
}

zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> AddChild(
    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent, fdf::Logger& logger,
    std::string_view node_name, const fuchsia_driver_framework::NodePropertyVector& properties,
    const std::vector<fuchsia_driver_framework::Offer>& offers) {
  auto [node_controller_client_end, node_controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  fuchsia_driver_framework::NodeAddArgs args{{
      .name = {std::string(node_name)},
      .properties = properties,
      .offers2 = offers,
  }};

  fidl::Result<fuchsia_driver_framework::Node::AddChild> result =
      fidl::Call(parent)->AddChild({std::move(args), std::move(node_controller_server_end), {}});

  if (result.is_error()) {
    FDF_LOGL(ERROR, logger, "Failed to add child %s. Error: %s", std::string(node_name).c_str(),
             result.error_value().FormatDescription().c_str());
    return zx::error(result.error_value().is_framework_error()
                         ? result.error_value().framework_error().status()
                         : ZX_ERR_INTERNAL);
  }

  return zx::ok(std::move(node_controller_client_end));
}

zx::result<OwnedChildNode> AddOwnedChild(
    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent, fdf::Logger& logger,
    std::string_view node_name, fuchsia_driver_framework::DevfsAddArgs& devfs_args) {
  auto [node_controller_client_end, node_controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  auto [node_client_end, node_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::Node>::Create();

  fuchsia_driver_framework::NodeAddArgs args{{
      .name = {std::string(node_name)},
      .devfs_args = std::move(devfs_args),
  }};

  fidl::Result<fuchsia_driver_framework::Node::AddChild> result = fidl::Call(parent)->AddChild(
      {std::move(args), std::move(node_controller_server_end), std::move(node_server_end)});

  if (result.is_error()) {
    FDF_LOGL(ERROR, logger, "Failed to add owned devfs child %s. Error: %s",
             std::string(node_name).c_str(), result.error_value().FormatDescription().c_str());
    return zx::error(result.error_value().is_framework_error()
                         ? result.error_value().framework_error().status()
                         : ZX_ERR_INTERNAL);
  }

  return zx::ok(OwnedChildNode{std::move(node_controller_client_end), std::move(node_client_end)});
}

zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> AddChild(
    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent, fdf::Logger& logger,
    std::string_view node_name, fuchsia_driver_framework::DevfsAddArgs& devfs_args,
    const fuchsia_driver_framework::NodePropertyVector& properties,
    const std::vector<fuchsia_driver_framework::Offer>& offers) {
  auto [node_controller_client_end, node_controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  fuchsia_driver_framework::NodeAddArgs args{{
      .name = {std::string(node_name)},
      .properties = properties,
      .devfs_args = std::move(devfs_args),
      .offers2 = offers,
  }};

  fidl::Result<fuchsia_driver_framework::Node::AddChild> result =
      fidl::Call(parent)->AddChild({std::move(args), std::move(node_controller_server_end), {}});

  if (result.is_error()) {
    FDF_LOGL(ERROR, logger, "Failed to add devfs child %s. Error: %s",
             std::string(node_name).c_str(), result.error_value().FormatDescription().c_str());
    return zx::error(result.error_value().is_framework_error()
                         ? result.error_value().framework_error().status()
                         : ZX_ERR_INTERNAL);
  }

  return zx::ok(std::move(node_controller_client_end));
}

}  // namespace fdf

#endif
