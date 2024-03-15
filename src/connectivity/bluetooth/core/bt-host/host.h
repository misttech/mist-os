// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_HOST_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_HOST_H_

#include <lib/async/dispatcher.h>
#include <lib/inspect/component/cpp/component.h>

#include <pw_async_fuchsia/dispatcher.h>

#include "fidl/fuchsia.hardware.bluetooth/cpp/fidl.h"
#include "fuchsia/hardware/bluetooth/cpp/fidl.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gap/adapter.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/gatt/gatt.h"
#include "third_party/pigweed/backends/pw_random/zircon_random_generator.h"

namespace bthost {

class HostServer;

class BtHostComponent {
 public:
  // Creates a new Host.
  static std::unique_ptr<BtHostComponent> Create(async_dispatcher_t* dispatcher,
                                                 const std::string& device_path);

  // Does not override RNG
  static std::unique_ptr<BtHostComponent> CreateForTesting(async_dispatcher_t* dispatcher,
                                                           const std::string& device_path);

  ~BtHostComponent();

  // Initializes the system and reports the status to the |init_cb| in |success|.
  // |error_cb| will be called if a transport error occurs in the Host after initialization. Returns
  // false if initialization fails, otherwise returns true.
  using InitCallback = fit::callback<void(bool success)>;
  using ErrorCallback = fit::callback<void()>;
  [[nodiscard]] bool Initialize(fuchsia::hardware::bluetooth::VendorHandle vendor_handle,
                                InitCallback init_cb, ErrorCallback error_cb);

  // Shuts down all systems.
  void ShutDown();

  // Binds the given |host_client| to a Host FIDL interface server.
  void BindToHostInterface(fidl::ServerEnd<fuchsia_bluetooth_host::Host> host_client);

  std::string device_path() { return device_path_; }

  using WeakPtr = WeakSelf<BtHostComponent>::WeakPtr;
  WeakPtr GetWeakPtr() { return weak_self_.GetWeakPtr(); }

 private:
  BtHostComponent(async_dispatcher_t* dispatcher, const std::string& device_path,
                  bool initialize_rng);

  pw::async::fuchsia::FuchsiaDispatcher pw_dispatcher_;

  // Path of bt-hci device the component supports
  std::string device_path_;

  bool initialize_rng_;

  pw_random_zircon::ZirconRandomGenerator random_generator_;

  std::unique_ptr<bt::hci::Transport> hci_;

  std::unique_ptr<bt::gap::Adapter> gap_;

  // The GATT profile layer and bus.
  std::unique_ptr<bt::gatt::GATT> gatt_;

  // Currently connected Host interface handle.
  // A Host allows only one of these to be connected at a time.
  std::unique_ptr<HostServer> host_server_;

  // Inspector for component inspect tree. This object is thread-safe.
  inspect::ComponentInspector inspector_;

  WeakSelf<BtHostComponent> weak_self_{this};

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(BtHostComponent);
};

}  // namespace bthost

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_HOST_H_
