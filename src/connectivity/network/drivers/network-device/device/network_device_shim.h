// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_NETWORK_DEVICE_SHIM_H_
#define SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_NETWORK_DEVICE_SHIM_H_

#include <fidl/fuchsia.hardware.network.driver/cpp/driver/fidl.h>
#include <fuchsia/hardware/network/driver/cpp/banjo.h>

#include <optional>

#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>

#include "network_port_shim.h"
#include "public/network_device.h"

namespace network {

namespace netdriver = fuchsia_hardware_network_driver;

// Translates calls between the parent device and the underlying netdevice.
//
// Usage of this type assumes that the parent device speaks Banjo while the underlying netdevice
// speaks FIDL. Consequently, this type translates calls from netdevice into the parent from FIDL to
// Banjo and calls from the parent into netdevice from Banjo to FIDL.
class NetworkDeviceShim : public fdf::WireServer<netdriver::NetworkDeviceImpl>,
                          public ddk::NetworkDeviceIfcProtocol<NetworkDeviceShim>,
                          public NetworkDeviceImplBinder {
 public:
  NetworkDeviceShim(ddk::NetworkDeviceImplProtocolClient impl, const ShimDispatchers& dispatchers);
  ~NetworkDeviceShim() override;

  // NetworkDeviceImplBinder implementation
  zx::result<fdf::ClientEnd<netdriver::NetworkDeviceImpl>> Bind() override;
  Synchronicity Teardown(fit::callback<void()>&& on_teardown_complete) override;

  // NetworkDeviceImpl wire implementation
  void Init(netdriver::wire::NetworkDeviceImplInitRequest* request, fdf::Arena& arena,
            InitCompleter::Sync& completer) override;
  void Start(fdf::Arena& arena, StartCompleter::Sync& completer) override;
  void Stop(fdf::Arena& arena, StopCompleter::Sync& completer) override;
  void GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) override;
  void QueueTx(netdriver::wire::NetworkDeviceImplQueueTxRequest* request, fdf::Arena& arena,
               QueueTxCompleter::Sync& completer) override;
  void QueueRxSpace(netdriver::wire::NetworkDeviceImplQueueRxSpaceRequest* request,
                    fdf::Arena& arena, QueueRxSpaceCompleter::Sync& completer) override;
  void PrepareVmo(netdriver::wire::NetworkDeviceImplPrepareVmoRequest* request, fdf::Arena& arena,
                  PrepareVmoCompleter::Sync& completer) override;
  void ReleaseVmo(netdriver::wire::NetworkDeviceImplReleaseVmoRequest* request, fdf::Arena& arena,
                  ReleaseVmoCompleter::Sync& completer) override;

  // NetworkDeviceIfc implementation.
  void NetworkDeviceIfcPortStatusChanged(uint8_t port_id, const port_status_t* new_status);
  void NetworkDeviceIfcAddPort(uint8_t port_id, const network_port_protocol_t* port,
                               network_device_ifc_add_port_callback callback, void* cookie);
  void NetworkDeviceIfcRemovePort(uint8_t port_id);
  void NetworkDeviceIfcCompleteRx(const rx_buffer_t* rx_list, size_t rx_count);
  void NetworkDeviceIfcCompleteTx(const tx_result_t* tx_list, size_t tx_count);
  void NetworkDeviceIfcDelegateRxLease(const delegated_rx_lease_t* delegated);

 private:
  using NetworkPortShimList = fbl::SizedDoublyLinkedList<std::unique_ptr<NetworkPortShim>>;

  void MaybeFinishTeardown() __TA_REQUIRES(lock_) __TA_RELEASE(lock_);

  ddk::NetworkDeviceImplProtocolClient impl_;
  std::optional<fdf::ServerBindingRef<netdriver::NetworkDeviceImpl>> binding_ __TA_GUARDED(lock_);
  fit::callback<void()> on_teardown_complete_ __TA_GUARDED(lock_);
  NetworkPortShimList network_port_shims_ __TA_GUARDED(lock_);
  fbl::Mutex lock_;

  const ShimDispatchers dispatchers_;
  fdf::WireSharedClient<netdriver::NetworkDeviceIfc> device_ifc_;
};

}  // namespace network

#endif  // SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_NETWORK_DEVICE_SHIM_H_
