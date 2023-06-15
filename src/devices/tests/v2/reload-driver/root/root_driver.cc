// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/composite_node_spec.h>

#include <bind/fuchsia/reloaddriverbind/test/cpp/bind.h>

#include "src/devices/tests/v2/reload-driver/driver_helpers.h"

namespace bindlib = bind_fuchsia_reloaddriverbind_test;
namespace helpers = reload_test_driver_helpers;

namespace {

class RootDriver : public fdf::DriverBase {
 public:
  RootDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("root", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    node_client_.Bind(std::move(node()));

    zx::result result =
        helpers::AddChild(logger(), "B", node_client_, bindlib::TEST_BIND_PROPERTY_LEFT_PARENT);
    if (result.is_error()) {
      return result.take_error();
    }
    node_controller_1_.Bind(std::move(result.value()));

    result =
        helpers::AddChild(logger(), "C", node_client_, bindlib::TEST_BIND_PROPERTY_RIGHT_PARENT);
    if (result.is_error()) {
      return result.take_error();
    }
    node_controller_2_.Bind(std::move(result.value()));

    zx::result spec_result = MakeCompositeSpec();
    if (spec_result.is_error()) {
      return spec_result.take_error();
    }

    return helpers::SendAck(logger(), node_name().value_or("None"), incoming(), name());
  }

 private:
  zx::result<> MakeCompositeSpec() {
    auto parent_d = fuchsia_driver_framework::ParentSpec{{
        .bind_rules =
            {
                fdf::MakeAcceptBindRule(bindlib::TEST_BIND_PROPERTY,
                                        bindlib::TEST_BIND_PROPERTY_COMPOSITE_PARENT_D),
            },
        .properties =
            {
                fdf::MakeProperty(bindlib::TEST_BIND_PROPERTY,
                                  bindlib::TEST_BIND_PROPERTY_COMPOSITE_PARENT_D),
            },
    }};

    auto parent_e = fuchsia_driver_framework::ParentSpec{{
        .bind_rules =
            {
                fdf::MakeAcceptBindRule(bindlib::TEST_BIND_PROPERTY,
                                        bindlib::TEST_BIND_PROPERTY_COMPOSITE_PARENT_E),
            },
        .properties =
            {
                fdf::MakeProperty(bindlib::TEST_BIND_PROPERTY,
                                  bindlib::TEST_BIND_PROPERTY_COMPOSITE_PARENT_E),
            },
    }};

    auto parent_f = fuchsia_driver_framework::ParentSpec{{
        .bind_rules =
            {
                fdf::MakeAcceptBindRule(bindlib::TEST_BIND_PROPERTY,
                                        bindlib::TEST_BIND_PROPERTY_COMPOSITE_PARENT_F),
            },
        .properties =
            {
                fdf::MakeProperty(bindlib::TEST_BIND_PROPERTY,
                                  bindlib::TEST_BIND_PROPERTY_COMPOSITE_PARENT_F),
            },
    }};

    auto spec = fuchsia_driver_framework::CompositeNodeSpec{{
        .name = "H",
        .parents = {{
            parent_d,
            parent_e,
            parent_f,
        }},
    }};

    auto cnm_client = incoming()->Connect<fuchsia_driver_framework::CompositeNodeManager>();
    if (cnm_client.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to CompositeNodeManager: %s",
              zx_status_get_string(cnm_client.error_value()));
      return cnm_client.take_error();
    }

    fidl::SyncClient<fuchsia_driver_framework::CompositeNodeManager> composite_node_manager(
        std::move(cnm_client.value()));
    auto result = composite_node_manager->AddSpec(spec);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to AddSpec: %s", result.error_value().FormatDescription().c_str());
      return zx::error(ZX_ERR_INTERNAL);
    }

    return zx::ok();
  }
  fidl::SyncClient<fuchsia_driver_framework::Node> node_client_;
  fidl::SyncClient<fuchsia_driver_framework::NodeController> node_controller_1_;
  fidl::SyncClient<fuchsia_driver_framework::NodeController> node_controller_2_;
};

}  // namespace

FUCHSIA_DRIVER_EXPORT(RootDriver);
