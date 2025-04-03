// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_DEVICE_PORT_H_
#define VENDOR_MISTTECH_SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_DEVICE_PORT_H_

// #include <lib/async/dispatcher.h>
#include <lib/fit/function.h>
#include <lib/stdcompat/optional.h>

#include <fbl/mutex.h>

#include "status_watcher.h"
#include "vendor/misttech/src/connectivity/network/drivers/network-device/mac/public/network_mac.h"

namespace network::internal {

class DeviceInterface;

class DevicePort /*: public fidl::WireServer<netdev::Port>*/ {
 public:
  using TeardownCallback = fit::callback<void(DevicePort&)>;
  using OnCreated = fit::callback<void(zx::result<std::unique_ptr<DevicePort>>)>;

  // Asynchronously create a DevicePort object. This will make calls on the port_client to determine
  // the initial state of the port. Once the port object is ready it will be provided through the
  // on_created callback. If an error occurs it will also be reported through on_created. Note that
  // on an error the callback may be (but is not guaranteed to be) called inline from the Create
  // call. Because of this it's a good idea to avoid acquiring locks in the on_created callback when
  // an error is reported.
  static void Create(DeviceInterface* parent, port_id_t id,
                     ddk::NetworkPortProtocolClient port_client, TeardownCallback&& on_teardown,
                     OnCreated&& on_created);

  ~DevicePort();

  port_id_t id() const { return id_; }

  struct Counters {
    ktl::atomic<uint64_t> rx_frames = 0;
    ktl::atomic<uint64_t> rx_bytes = 0;
    ktl::atomic<uint64_t> tx_frames = 0;
    ktl::atomic<uint64_t> tx_bytes = 0;
  };

  // Notifies port of status changes notifications from the network device implementation.
  void StatusChanged(const port_status_t& new_status);
  // Starts port teardown process.
  //
  // Once port is torn down and ready to be deleted, the teardown callback passed on construction
  // will be called.
  // Calling teardown while a teardown is already in progress is a no-op.
  void Teardown();
  // Notifies the port a session attached to it.
  //
  // When sessions attach to a port, the port will notify the network port implementation that the
  // port is active.
  void SessionAttached();
  // Notifies the port a session detached from it.
  //
  // When all sessions are detached from a port, the port will notify the network port
  // implementation that the port is inactive.
  void SessionDetached();

  // Binds a new FIDL request to this port.
  // void Bind(fidl::ServerEnd<netdev::Port> req);
  // void Bind(ddk::NetworkPortProtocolClient* client) { *client = port_; }

  // Returns true if `frame_type` is a valid inbound frame type for subscription to on this port.
  bool IsValidRxFrameType(frame_type_t frame_type) const;
  // Returns true if `frame_type` is a valid outbound frame type for this port.
  bool IsValidTxFrameType(frame_type_t frame_type) const;

  // FIDL protocol implementation.
  void GetInfo(port_base_info_t* info);
  void GetStatus(port_status_t* status);
  StatusWatcher* GetStatusWatcher(uint32_t buffer);
  void GetMac(mac_addr_protocol_t** out_mac_ifc);
  // void GetDevice(GetDeviceRequestView request, GetDeviceCompleter::Sync& _completer) override;
  // void Clone(CloneRequestView request, CloneCompleter::Sync& _completer) override;
  // void GetCounters(GetCountersCompleter::Sync& completer) override;
  // void GetDiagnostics(GetDiagnosticsRequestView request,
  //                    GetDiagnosticsCompleter::Sync& _completer) override;

  Counters& counters() { return counters_; }

 private:
#if 0
  // Helper class to keep track of clients bound to DevicePort.
  class Binding : public fbl::DoublyLinkedListable<std::unique_ptr<Binding>> {
   public:
    Binding() = default;
    void Unbind() {
      if (binding_.has_value()) {
        binding_->Unbind();
      }
    }
    void Bind(fidl::ServerBindingRef<netdev::Port> binding) { binding_ = std::move(binding); }

   private:
    std::optional<fidl::ServerBindingRef<netdev::Port>> binding_;
  };

  using BindingList = fbl::DoublyLinkedList<std::unique_ptr<Binding>>;
#endif

  DevicePort(DeviceInterface* parent, port_id_t id, ddk::NetworkPortProtocolClient port,
             TeardownCallback&& on_teardown);

  void Init(fit::callback<void(zx_status_t)>&& on_complete);
  void GetMac(fit::callback<void(zx::result<ddk::MacAddrProtocolClient>)>&& on_complete);
  void CreateMacInterface(ddk::MacAddrProtocolClient&& client_end,
                          fit::callback<void(zx_status_t)>&& on_complete);
  void GetInitialPortInfo(fit::callback<void(zx_status_t)>&& on_complete);
  void GetInitialStatus(fit::callback<void(zx_status_t)>&& on_complete);

  // Concludes an ongoing teardown if it is ongoing and all internal resources are released.
  //
  // Returns true if teardown callback was dispatched.
  // Callers must assume the port is destroyed immediately if this function returns true.
  bool MaybeFinishTeardown() __TA_REQUIRES(lock_);
  // Implements session attachment and detachment.
  //
  // Notifies network port implementation that the port is active when `new_count` is 1.
  // Notifies network port implementation that the port is inactive when `new_count` is 0.
  // No-op otherwise.
  void NotifySessionCount(size_t new_count) __TA_REQUIRES(lock_);

  DeviceInterface* const parent_;  // Pointer to parent device. Not owned.
  const port_id_t id_;
  ddk::NetworkPortProtocolClient port_;
  Counters counters_;
  std::unique_ptr<MacAddrDeviceInterface> mac_ __TA_GUARDED(lock_);
  //   BindingList bindings_ __TA_GUARDED(lock_);

  port_class_t port_class_;
  port_status_t status_;
  fbl::Vector<frame_type_t> supported_rx_;
  fbl::Vector<frame_type_support_t> supported_tx_;

  fbl::Mutex lock_;
  StatusWatcherList watchers_ __TA_GUARDED(lock_);
  TeardownCallback on_teardown_ __TA_GUARDED(lock_);
  bool teardown_started_ __TA_GUARDED(lock_) = false;
  size_t attached_sessions_count_ __TA_GUARDED(lock_) = 0;

  DISALLOW_COPY_ASSIGN_AND_MOVE(DevicePort);
};

}  // namespace network::internal

#endif  // VENDOR_MISTTECH_SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_DEVICE_PORT_H_
