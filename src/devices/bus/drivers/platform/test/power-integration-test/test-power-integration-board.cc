// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/platform/test/power-integration-test/test-power-integration-board.h"

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/test/platform/cpp/bind.h>

namespace power_integration_board {

zx::result<> PowerIntegrationBoard::Start() {
  zx::result<fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>> client =
      incoming()->Connect<fuchsia_hardware_platform_bus::Service::PlatformBus>();

  if (client.is_error()) {
    return zx::error_result(ZX_ERR_ACCESS_DENIED);
  }

  fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus> &platform_bus = client.value();

  fdf::Arena arena('TEST');
  auto board_info = platform_bus.buffer(arena)->GetBoardInfo();

  if (board_info->is_error()) {
    return zx::error_result(ZX_ERR_IO);
  }

  fuchsia_hardware_power::PowerElementConfiguration config;

  // create the composite node for the child
  {
    fuchsia_hardware_power::Transition to_on;
    to_on.latency_us() = 350;
    to_on.target_level() = 1;

    fuchsia_hardware_power::PowerLevel off = fuchsia_hardware_power::PowerLevel();
    off.level() = 0;
    off.name() = "off";
    off.transitions() = std::vector<fuchsia_hardware_power::Transition>{to_on};

    fuchsia_hardware_power::Transition to_off;
    to_off.latency_us() = 20;
    to_off.target_level() = 0;

    fuchsia_hardware_power::PowerLevel on;
    on.name() = "on";
    on.level() = 1;
    on.transitions() = std::vector<fuchsia_hardware_power::Transition>{to_off};

    fuchsia_hardware_power::PowerElement element;
    element.name() = "pe-fake-child";
    element.levels() = std::vector<fuchsia_hardware_power::PowerLevel>{on, off};

    fuchsia_hardware_power::PowerElementConfiguration config;
    config.element() = element;

    // set up dependency between parent and child
    fuchsia_hardware_power::PowerDependency dep;
    dep.child() = "pe-fake-child";
    dep.parent() = fuchsia_hardware_power::ParentElement::WithName("pe-fake-parent");
    dep.level_deps() = std::vector<fuchsia_hardware_power::LevelTuple>{{{
        .child_level = 1,
        .parent_level = 1,
    }}};
    dep.strength() = fuchsia_hardware_power::RequirementType::kActive;
    config.dependencies() = std::vector<fuchsia_hardware_power::PowerDependency>{dep};

    std::vector<fuchsia_hardware_power::PowerElementConfiguration> power_config =
        std::vector<fuchsia_hardware_power::PowerElementConfiguration>{config};

    fuchsia_hardware_platform_bus::Node pdev;
    pdev.name() = "composite_fake_child";
    pdev.vid() = bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST;
    pdev.pid() = bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_POWER_TEST;
    pdev.did() = bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_FAKE_POWER_PLATFORM_DEVICE_CHILD;
    pdev.power_config() = power_config;

    auto bind_rules = std::vector{
        fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                                bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST),
        fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_PID,
                                bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_POWER_TEST),
        fdf::MakeAcceptBindRule(
            bind_fuchsia::PLATFORM_DEV_DID,
            bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_FAKE_POWER_CHILD)};

    auto properties = std::vector{
        fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                          bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST),
        fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_PID,
                          bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_POWER_TEST),
        fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                          bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_FAKE_POWER_CHILD)};

    auto parents = std::vector{
        fuchsia_driver_framework::ParentSpec{{.bind_rules = bind_rules, .properties = properties}}};

    auto composite_node_spec = fuchsia_driver_framework::CompositeNodeSpec{{
        .name = "composite_fake_child",
        .parents = parents,
    }};

    auto result = platform_bus.buffer(arena)->AddCompositeNodeSpec(
        fidl::ToWire(arena, pdev), fidl::ToWire(arena, composite_node_spec));

    if (!result.ok()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    if (result->is_error()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
  }

  {
    fuchsia_hardware_platform_bus::Node parent_pdev;
    parent_pdev.name() = "pdev_parent";
    parent_pdev.vid() = bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST;
    parent_pdev.pid() = bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_POWER_TEST;
    parent_pdev.did() =
        bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_FAKE_POWER_PLATFORM_DEVICE_PARENT;

    fuchsia_hardware_power::PowerLevel off;
    off.level() = 0;
    off.name() = "off";
    off.transitions() =
        std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
            .target_level = 1,
            .latency_us = 200,
        }}};

    fuchsia_hardware_power::PowerLevel on;
    on.level() = 1;
    on.name() = "on";
    on.transitions() =
        std::vector<fuchsia_hardware_power::Transition>{fuchsia_hardware_power::Transition{{
            .target_level = 0,
            .latency_us = 5,
        }}};

    fuchsia_hardware_power::PowerElementConfiguration config;
    config.element() = fuchsia_hardware_power::PowerElement{{
        .name = "pe-fake-parent",
        .levels =
            std::vector<fuchsia_hardware_power::PowerLevel>{
                on,
                off,
            },

    }};

    parent_pdev.power_config() = std::vector<fuchsia_hardware_power::PowerElementConfiguration>{
        config,
    };

    auto bind_rules = std::vector{
        fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                                bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST),
        fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_PID,
                                bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_POWER_TEST),
        fdf::MakeAcceptBindRule(
            bind_fuchsia::PLATFORM_DEV_DID,
            bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_FAKE_POWER_PARENT)};

    auto properties = std::vector{
        fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                          bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST),
        fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_PID,
                          bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_POWER_TEST),
        fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                          bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_FAKE_POWER_PARENT)};

    auto parents = std::vector{
        fuchsia_driver_framework::ParentSpec{{.bind_rules = bind_rules, .properties = properties}}};

    auto composite_node_spec = fuchsia_driver_framework::CompositeNodeSpec{{
        .name = "composite_fake_parent",
        .parents = parents,
    }};

    auto result = platform_bus.buffer(arena)->AddCompositeNodeSpec(
        fidl::ToWire(arena, parent_pdev), fidl::ToWire(arena, composite_node_spec));

    if (!result.ok()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    if (result->is_error()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
  }

  // Create the parent device
  {
    fuchsia_hardware_platform_bus::Node fake_parent_device;
    fake_parent_device.name() = "fake_parent";
    fake_parent_device.vid() = bind_fuchsia_test_platform::BIND_PLATFORM_DEV_VID_TEST;
    fake_parent_device.pid() = bind_fuchsia_test_platform::BIND_PLATFORM_DEV_PID_POWER_TEST;
    fake_parent_device.did() = bind_fuchsia_test_platform::BIND_PLATFORM_DEV_DID_FAKE_POWER_PARENT;

    auto result = platform_bus.buffer(arena)->NodeAdd(fidl::ToWire(arena, fake_parent_device));

    if (!result.ok()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    if (result->is_error()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
  }

  return zx::ok();
}

}  // namespace power_integration_board

FUCHSIA_DRIVER_EXPORT(power_integration_board::PowerIntegrationBoard);
