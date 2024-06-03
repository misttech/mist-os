// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "test_root.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

#include <bind/fuchsia_driver_metadata_test_bind_library/cpp/bind.h>

namespace fdf_metadata::test {

zx::result<> TestRoot::Start() {
  node_.Bind(std::move(node()));

  zx::result result = AddTestParentNode(std::string(kTestParentExposeNodeName),
                                        bind_fuchsia_driver_metadata_test::TEST_PARENT_EXPOSE);
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add test parent node", KV("name", kTestParentExposeNodeName),
             KV("status", result.status_string()));
    return result.take_error();
  }
  test_parent_expose_controller_.emplace(std::move(result.value()));

  result = AddTestParentNode(std::string(kTestParentNoExposeNodeName),
                             bind_fuchsia_driver_metadata_test::TEST_PARENT_NO_EXPOSE);
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add test parent node", KV("name", kTestParentNoExposeNodeName),
             KV("status", result.status_string()));
    return result.take_error();
  }
  test_parent_no_expose_controller_.emplace(std::move(result.value()));

  return zx::ok();
}

zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> TestRoot::AddTestParentNode(
    std::string node_name, std::string test_parent_property_value) {
  std::vector<fuchsia_driver_framework::NodeProperty> properties{
      {{.key = fuchsia_driver_framework::NodePropertyKey::WithStringValue(
            bind_fuchsia_driver_metadata_test::TEST_PARENT),
        .value = fuchsia_driver_framework::NodePropertyValue::WithEnumValue(
            std::move(test_parent_property_value))}}};

  fuchsia_driver_framework::NodeAddArgs args{
      {.name = std::move(node_name), .properties = std::move(properties)}};

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_SLOG(ERROR, "Failed to create node controller endpoints",
             KV("status", controller_endpoints.status_string()));
    return controller_endpoints.take_error();
  }

  fidl::Result result =
      node_->AddChild({std::move(args), std::move(controller_endpoints->server), {}});
  if (result.is_error()) {
    auto& error = result.error_value();
    FDF_SLOG(ERROR, "Failed to add child", KV("status", error.FormatDescription()));
    return zx::error(error.is_domain_error() ? ZX_ERR_INTERNAL : error.framework_error().status());
  }

  return zx::ok(std::move(controller_endpoints->client));
}

}  // namespace fdf_metadata::test

FUCHSIA_DRIVER_EXPORT(fdf_metadata::test::TestRoot);
