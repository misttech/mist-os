// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/virtual/controller.h"

#include <lib/driver/component/cpp/driver_export.h>

#include <memory>
#include <string>

namespace bt_hci_virtual {

VirtualController::VirtualController(fdf::DriverStartArgs start_args,
                                     fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("bt_hci_virtual", std::move(start_args), std::move(driver_dispatcher)),
      node_(fidl::WireClient(std::move(node()), dispatcher())),
      devfs_connector_(fit::bind_member<&VirtualController::Connect>(this)) {}

zx::result<> VirtualController::Start() {
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Failed to bind devfs connecter to dispatcher: %u", connector.status_value());
    return connector.take_error();
  }

  fidl::Arena args_arena;
  // TODO(https://fxbug.dev/303503457): Access virtual device via "/dev/class/bt-hci-virtual"
  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(args_arena)
                   .connector(std::move(connector.value()))
                   .class_name("sys/platform/00:00:30")
                   .Build();
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(args_arena)
                  .name("bt_hci_virtual")
                  .devfs_args(devfs)
                  .Build();

  // Add bt_hci_virtual child node
  AddVirtualControllerChildNode(args);

  return zx::ok();
}

void VirtualController::CreateEmulator(CreateEmulatorCompleter::Sync& completer) {
  std::string name = "emulator";

  emulator_device_ = std::make_unique<EmulatorDevice>();

  auto add_child_cb = [this](auto args) {
    FDF_LOG(INFO, "EmulatorDevice successfully initialized");
    AddEmulatorChildNode(args, emulator_device_.get());
  };
  auto shutdown_cb = [this]() {
    FDF_LOG(INFO, "Releasing EmulatorDevice");
    emulator_device_.reset();
  };

  zx_status_t status = emulator_device_->Initialize(std::string_view(name), std::move(add_child_cb),
                                                    std::move(shutdown_cb));

  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to bind: %s\n", zx_status_get_string(status));
    emulator_device_->Shutdown();
    auto _ = emulator_node_controller_->Remove();
    completer.ReplyError(status);
    return;
  }
  emulator_device_->set_emulator_child_node(std::move(emulator_child_node_));
  completer.ReplySuccess(fidl::StringView::FromExternal(name.data(), name.size()));
}

void VirtualController::CreateLoopbackDevice(CreateLoopbackDeviceRequestView request,
                                             CreateLoopbackDeviceCompleter::Sync& completer) {
  std::string name = "loopback";

  loopback_device_ = std::make_unique<LoopbackDevice>();

  zx_handle_t channel = request->channel.release();
  zx_status_t status =
      loopback_device_->Initialize(channel, std::string_view(name), [this](auto args) {
        // Add LoopbackDevice as a child node of VirtualController
        FDF_LOG(INFO, "LoopbackDevice successfully initialized");
        AddLoopbackChildNode(args);
      });
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to bind: %s\n", zx_status_get_string(status));
    loopback_device_->Shutdown();
    auto _ = loopback_node_controller_->Remove();
    return;
  }
}

void VirtualController::Connect(
    fidl::ServerEnd<fuchsia_hardware_bluetooth::VirtualController> request) {
  virtual_controller_binding_group_.AddBinding(dispatcher(), std::move(request), this,
                                               fidl::kIgnoreBindingClosure);
}

zx_status_t VirtualController::AddVirtualControllerChildNode(
    fuchsia_driver_framework::wire::NodeAddArgs args) {
  // Create the endpoints of fuchsia_driver_framework::NodeController protocol
  auto controller_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_LOG(ERROR, "Create node controller endpoints failed: %s",
            controller_endpoints.status_string());
    return controller_endpoints.error_value();
  }

  // Create the endpoints of fuchsia_driver_framework::Node protocol for the child node, and hold
  // the client end of it, because no driver will bind to the child node.
  auto child_node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (child_node_endpoints.is_error()) {
    FDF_LOG(ERROR, "Create child node endpoints failed: %s", child_node_endpoints.status_string());
    return child_node_endpoints.error_value();
  }

  // Add a VirtualController child node
  auto child_result =
      node_.sync()->AddChild(std::move(args), std::move(controller_endpoints->server),
                             std::move(child_node_endpoints->server));
  if (!child_result.ok()) {
    FDF_LOG(ERROR, "Failed to add bt_hci_virtual child node, FIDL error: %s",
            child_result.status_string());
    return child_result.status();
  }
  if (child_result->is_error()) {
    FDF_LOG(ERROR, "Failed to add bt_hci_virtual child node: %u",
            static_cast<uint32_t>(child_result->error_value()));
    return ZX_ERR_INTERNAL;
  }

  virtual_controller_child_node_.Bind(std::move(child_node_endpoints->client), dispatcher(), this);
  node_controller_.Bind(std::move(controller_endpoints->client), dispatcher(), this);

  return ZX_OK;
}

zx_status_t VirtualController::AddLoopbackChildNode(
    fuchsia_driver_framework::wire::NodeAddArgs args) {
  // Create the endpoints of fuchsia_driver_framework::NodeController protocol
  auto controller_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_LOG(ERROR, "Create node controller endpoints failed: %s",
            controller_endpoints.status_string());
    return controller_endpoints.error_value();
  }

  // Create the endpoints of fuchsia_driver_framework::Node protocol for the child node, and hold
  // the client end of it, because no driver will bind to the child node.
  auto child_node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (child_node_endpoints.is_error()) {
    FDF_LOG(ERROR, "Create child node endpoints failed: %s", child_node_endpoints.status_string());
    return child_node_endpoints.error_value();
  }

  // Add a loopback device as a child node of VirtualController
  auto child_result =
      node_.sync()->AddChild(std::move(args), std::move(controller_endpoints->server),
                             std::move(child_node_endpoints->server));
  if (!child_result.ok()) {
    FDF_LOG(ERROR, "Failed to add loopback device node, FIDL error: %s",
            child_result.status_string());
    return child_result.status();
  }

  if (child_result->is_error()) {
    FDF_LOG(ERROR, "Failed to add loopback device node: %u",
            static_cast<uint32_t>(child_result->error_value()));
    return ZX_ERR_INTERNAL;
  }

  // |loopback_child_node_| does not need to create more child nodes so we do not need an
  // event_handler and we do not need to worry about it being re-bound
  loopback_child_node_.Bind(std::move(child_node_endpoints->client), dispatcher());
  loopback_node_controller_.Bind(std::move(controller_endpoints->client), dispatcher(), this);

  return ZX_OK;
}

zx_status_t VirtualController::AddEmulatorChildNode(
    fuchsia_driver_framework::wire::NodeAddArgs args, EmulatorDevice* emulator_device) {
  // Create the endpoints of fuchsia_driver_framework::NodeController protocol
  auto controller_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints.is_error()) {
    FDF_LOG(ERROR, "Create node controller endpoints failed: %s",
            controller_endpoints.status_string());
    return controller_endpoints.error_value();
  }

  // Create the endpoints of fuchsia_driver_framework::Node protocol for the child node, and hold
  // the client end of it, because no driver will bind to the child node.
  auto child_node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  if (child_node_endpoints.is_error()) {
    FDF_LOG(ERROR, "Create child node endpoints failed: %s", child_node_endpoints.status_string());
    return child_node_endpoints.error_value();
  }

  // Add an emulator device as a child node of VirtualController
  auto child_result =
      node_.sync()->AddChild(std::move(args), std::move(controller_endpoints->server),
                             std::move(child_node_endpoints->server));

  if (!child_result.ok()) {
    FDF_LOG(ERROR, "Failed to add emulator device node, FIDL error: %s",
            child_result.status_string());
    return child_result.status();
  }

  if (child_result->is_error()) {
    FDF_LOG(ERROR, "Failed to add emulator device node: %u",
            static_cast<uint32_t>(child_result->error_value()));
    return ZX_ERR_INTERNAL;
  }

  emulator_child_node_.Bind(std::move(child_node_endpoints->client), dispatcher(), emulator_device);
  emulator_node_controller_.Bind(std::move(controller_endpoints->client), dispatcher(), this);

  return ZX_OK;
}

}  // namespace bt_hci_virtual

FUCHSIA_DRIVER_EXPORT(bt_hci_virtual::VirtualController);
