// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_PASSTHROUGH_PASSTHROUGH_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_PASSTHROUGH_PASSTHROUGH_H_

#include <assert.h>
#include <fidl/fuchsia.hardware.bluetooth/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <zircon/status.h>

namespace bt::passthrough {

class PassthroughDevice
    : public fdf::DriverBase,
      public fidl::WireServer<fuchsia_hardware_bluetooth::Vendor>,
      public fidl::WireServer<fuchsia_hardware_bluetooth::HciTransport>,
      public fidl::WireAsyncEventHandler<fuchsia_hardware_bluetooth::HciTransport> {
 public:
  PassthroughDevice(fdf::DriverStartArgs start_args,
                    fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("bt_hci_passthrough", std::move(start_args), std::move(driver_dispatcher)),
        node_client_(fidl::WireClient(std::move(node()), dispatcher())),
        devfs_connector_(fit::bind_member<&PassthroughDevice::Connect>(this)) {}

 private:
  // DriverBase overrides:
  void Start(fdf::StartCompleter completer) override;
  void Stop() override;

  // WireServer<Vendor> overrides:
  void EncodeCommand(EncodeCommandRequestView request,
                     EncodeCommandCompleter::Sync& completer) override;
  void OpenHci(OpenHciCompleter::Sync& completer) override;
  void OpenHciTransport(OpenHciTransportCompleter::Sync& completer) override;
  void OpenSnoop(OpenSnoopCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::Vendor> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // WireServer<HciTransport> overrides:
  void Send(::fuchsia_hardware_bluetooth::wire::SentPacket* request,
            SendCompleter::Sync& completer) override;
  void AckReceive(AckReceiveCompleter::Sync& completer) override;
  void ConfigureSco(::fuchsia_hardware_bluetooth::wire::HciTransportConfigureScoRequest* request,
                    ConfigureScoCompleter::Sync& completer) override;
  void SetSnoop(::fuchsia_hardware_bluetooth::wire::HciTransportSetSnoopRequest* request,
                SetSnoopCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::HciTransport> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // WireAsyncEventHandler<HciTransport> overrides:
  void OnReceive(
      ::fidl::WireEvent<::fuchsia_hardware_bluetooth::HciTransport::OnReceive>* event) override;
  void on_fidl_error(::fidl::UnbindInfo error) override;
  void handle_unknown_event(
      fidl::UnknownEventMetadata<::fuchsia_hardware_bluetooth::HciTransport> metadata) override;

  // Called by devfs_connector_ when a client connects.
  void Connect(fidl::ServerEnd<fuchsia_hardware_bluetooth::Vendor> request);

  zx_status_t ConnectToHciTransportFidlProtocol();

  fidl::WireClient<fuchsia_hardware_bluetooth::HciTransport> hci_transport_client_;
  fidl::WireClient<fuchsia_driver_framework::Node> node_client_;
  fidl::WireClient<fuchsia_driver_framework::NodeController> child_node_controller_client_;
  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::Vendor> vendor_binding_group_;
  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::HciTransport> hci_transport_server_bindings_;
  driver_devfs::Connector<fuchsia_hardware_bluetooth::Vendor> devfs_connector_;
};

}  // namespace bt::passthrough

#endif  //  SRC_CONNECTIVITY_BLUETOOTH_HCI_PASSTHROUGH_PASSTHROUGH_H_
