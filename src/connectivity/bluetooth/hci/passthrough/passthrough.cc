// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "passthrough.h"

#include <lib/driver/component/cpp/driver_export.h>

namespace bt::passthrough {

void PassthroughDevice::Start(fdf::StartCompleter completer) {
  zx_status_t status = ConnectToHciTransportFidlProtocol();
  if (status != ZX_OK) {
    completer(zx::error(status));
    return;
  }

  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Failed to bind devfs connecter to dispatcher: %s", connector.status_string());
    completer(zx::error(ZX_ERR_INTERNAL));
    return;
  }

  fidl::Arena args_arena;
  auto devfs_add_args = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(args_arena)
                            .connector(std::move(connector.value()))
                            .class_name("bt-hci")
                            .Build();
  auto node_add_args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(args_arena)
                           .name("bt-hci-passthrough")
                           .devfs_args(devfs_add_args)
                           .Build();

  auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
  child_node_controller_client_.Bind(std::move(controller_endpoints.client), dispatcher());

  // Add bt_hci_passthrough child node
  node_client_->AddChild(node_add_args, std::move(controller_endpoints.server), {})
      .ThenExactlyOnce(
          [completer = std::move(completer)](
              fidl::WireUnownedResult<fuchsia_driver_framework::Node::AddChild>& result) mutable {
            if (!result.ok()) {
              FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
              completer(zx::error(result.status()));
              return;
            }

            FDF_LOG(INFO, "Started successfully");
            completer(zx::ok());
          });
}

void PassthroughDevice::Stop() {
  auto status = child_node_controller_client_->Remove();
  if (!status.ok()) {
    FDF_LOG(ERROR, "Could not remove child: %s", status.status_string());
  }
}

void PassthroughDevice::EncodeCommand(EncodeCommandRequestView request,
                                      EncodeCommandCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void PassthroughDevice::OpenHci(OpenHciCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void PassthroughDevice::OpenHciTransport(OpenHciTransportCompleter::Sync& completer) {
  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_bluetooth::HciTransport>();
  if (endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create endpoints: %s", zx_status_get_string(endpoints.error_value()));
    completer.ReplyError(endpoints.error_value());
    return;
  }

  hci_transport_server_bindings_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                            std::move(endpoints->server), this,
                                            fidl::kIgnoreBindingClosure);
  completer.ReplySuccess(std::move(endpoints->client));
}

void PassthroughDevice::OpenSnoop(OpenSnoopCompleter::Sync& completer) {
  zx::result<fidl::ClientEnd<fuchsia_hardware_bluetooth::Snoop2>> client_end =
      incoming()->Connect<fuchsia_hardware_bluetooth::HciService::Snoop>();
  if (client_end.is_error()) {
    FDF_LOG(ERROR, "Connect to Snoop2 protocol failed: %s", client_end.status_string());
    completer.ReplyError(client_end.error_value());
    return;
  }
  completer.ReplySuccess(std::move(client_end.value()));
}

void PassthroughDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in Vendor protocol, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void PassthroughDevice::Send(::fuchsia_hardware_bluetooth::wire::SentPacket* request,
                             SendCompleter::Sync& completer) {
  hci_transport_client_->Send(*request).ThenExactlyOnce(
      [completer = completer.ToAsync()](
          fidl::WireUnownedResult<fuchsia_hardware_bluetooth::HciTransport::Send>& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "Error forwarding HciTransport::Send: %s", result.status_string());
          completer.Close(result.status());
          return;
        }
        completer.Reply();
      });
}

void PassthroughDevice::AckReceive(AckReceiveCompleter::Sync& completer) {
  fidl::OneWayStatus status = hci_transport_client_->AckReceive();
  if (!status.ok()) {
    FDF_LOG(ERROR, "Error forwarding HciTransport::AckReceive: %s", status.status_string());
    completer.Close(status.status());
    return;
  }
}

void PassthroughDevice::ConfigureSco(
    ::fuchsia_hardware_bluetooth::wire::HciTransportConfigureScoRequest* request,
    ConfigureScoCompleter::Sync& completer) {
  fidl::OneWayStatus status = hci_transport_client_->ConfigureSco(*request);
  if (!status.ok()) {
    FDF_LOG(ERROR, "Error forwarding HciTransport::ConfigureSco: %s", status.status_string());
    completer.Close(status.status());
    return;
  }
}

void PassthroughDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::HciTransport> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown method in HciTransport protocol, closing with ZX_ERR_NOT_SUPPORTED");
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

void PassthroughDevice::OnReceive(
    ::fidl::WireEvent<::fuchsia_hardware_bluetooth::HciTransport::OnReceive>* event) {
  hci_transport_server_bindings_.ForEachBinding(
      [event](const fidl::ServerBinding<fuchsia_hardware_bluetooth::HciTransport>& binding) {
        fidl::Status status = fidl::WireSendEvent(binding)->OnReceive(*event);
        if (!status.ok()) {
          FDF_LOG(ERROR, "Failed to send OnReceive event to bt-host: %s", status.status_string());
        }
      });
}

void PassthroughDevice::on_fidl_error(::fidl::UnbindInfo error) {
  FDF_LOG(WARNING, "HciTransport FIDL error: %s", error.status_string());
}

void PassthroughDevice::handle_unknown_event(
    fidl::UnknownEventMetadata<::fuchsia_hardware_bluetooth::HciTransport> metadata) {
  FDF_LOG(WARNING, "Unknown event from HciTransport protocol");
}

void PassthroughDevice::Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request) {
  vendor_binding_group_.AddBinding(dispatcher(), std::move(request), this,
                                   fidl::kIgnoreBindingClosure);
  vendor_binding_group_.ForEachBinding(
      [](const fidl::ServerBinding<fuchsia_hardware_bluetooth::Vendor>& binding) {
        fidl::Arena arena;
        auto builder = fuchsia_hardware_bluetooth::wire::VendorFeatures::Builder(arena);
        fidl::Status status = fidl::WireSendEvent(binding)->OnFeatures(builder.Build());
        if (status.status() != ZX_OK) {
          FDF_LOG(ERROR, "Failed to send vendor features to bt-host: %s", status.status_string());
        }
      });
}

zx_status_t PassthroughDevice::ConnectToHciTransportFidlProtocol() {
  zx::result<fidl::ClientEnd<fuchsia_hardware_bluetooth::HciTransport>> client_end =
      incoming()->Connect<fuchsia_hardware_bluetooth::HciService::HciTransport>();
  if (client_end.is_error()) {
    FDF_LOG(ERROR, "Connect to HciTransport protocol failed: %s", client_end.status_string());
    return client_end.status_value();
  }

  hci_transport_client_ =
      fidl::WireClient(*std::move(client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                       /*event_handler=*/this);

  return ZX_OK;
}

}  // namespace bt::passthrough

FUCHSIA_DRIVER_EXPORT(bt::passthrough::PassthroughDevice);
