// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_NETWORK_DEVICE_H_
#define SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_NETWORK_DEVICE_H_

#include <fidl/fuchsia.hardware.network/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>

#include <memory>

#include "device/public/network_device.h"

namespace network {

class NetworkDevice;

// Creates `fuchsia_hardware_network_driver::NetworkDeviceImpl` endpoints for a
// parent device that is backed by the FIDL based driver runtime.
class FidlNetworkDeviceImplBinder : public NetworkDeviceImplBinder {
 public:
  explicit FidlNetworkDeviceImplBinder(std::weak_ptr<fdf::Namespace> incoming)
      : incoming_(std::move(incoming)) {}

  zx::result<fdf::ClientEnd<fuchsia_hardware_network_driver::NetworkDeviceImpl>> Bind() override;

 private:
  std::weak_ptr<fdf::Namespace> incoming_;
};

class NetworkDevice : public fdf::DriverBase,
                      public fidl::WireServer<fuchsia_hardware_network::DeviceInstance> {
 public:
  NetworkDevice(fdf::DriverStartArgs start_args,
                fdf::UnownedSynchronizedDispatcher driver_dispatcher);
  ~NetworkDevice() override;

  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  void GetDevice(GetDeviceRequestView request, GetDeviceCompleter::Sync& _completer) override;

  NetworkDeviceInterface* GetInterface() { return device_.get(); }

 private:
  void Connect(fidl::ServerEnd<fuchsia_hardware_network::DeviceInstance> request);
  zx::result<std::unique_ptr<NetworkDeviceImplBinder>> CreateImplBinder();

  std::unique_ptr<OwnedDeviceInterfaceDispatchers> dispatchers_;
  std::unique_ptr<OwnedShimDispatchers> shim_dispatchers_;

  std::unique_ptr<NetworkDeviceInterface> device_;

  // These are used for the child node created to enable discovery through devfs. The child node has
  // to be kept alive for the child to remain alive and discoverable through devfs.
  fidl::WireSyncClient<fuchsia_driver_framework::Node> child_node_;
  driver_devfs::Connector<fuchsia_hardware_network::DeviceInstance> devfs_connector_{
      fit::bind_member<&NetworkDevice::Connect>(this)};
  fidl::ServerBindingGroup<fuchsia_hardware_network::DeviceInstance> bindings_;
};

}  // namespace network

#endif  // SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_NETWORK_DEVICE_H_
