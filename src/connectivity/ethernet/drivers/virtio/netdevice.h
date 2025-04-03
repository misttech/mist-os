// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_ETHERNET_DRIVERS_VIRTIO_NETDEVICE_H_
#define SRC_CONNECTIVITY_ETHERNET_DRIVERS_VIRTIO_NETDEVICE_H_

#include <fuchsia/hardware/network/driver/cpp/banjo.h>
#include <fuchsia/net/c/banjo.h>
#include <lib/virtio/device.h>
#include <lib/virtio/ring.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <bitset>

#include <ddktl/device.h>
#include <fbl/macros.h>
#include <kernel/brwlock.h>
#include <virtio/net.h>

#include "src/lib/vmo_store/vmo_store.h"

namespace virtio {

using VmoStore = vmo_store::VmoStore<vmo_store::SlabStorage<uint32_t>>;

class NetworkDevice;

// using DeviceType = ddk::Device<NetworkDevice>;
class NetworkDevice : public Device,
                      // Mixins for protocol device:
                      // public DeviceType,
                      // Mixin for Network device banjo protocol:
                      public ddk::NetworkDeviceImplProtocol<NetworkDevice, ddk::base_protocol>,
                      public ddk::NetworkPortProtocol<NetworkDevice>,
                      public ddk::MacAddrProtocol<NetworkDevice> {
 public:
  class Buffer;
  class BufferData;

  // Specifies how many packets can fit in each of the receive and transmit
  // backlogs.
  // Chosen arbitrarily. Larger values will cause increased memory consumption,
  // lower values may cause ring underruns.
  static constexpr uint16_t kMaxDepth = 256;
  // The single port ID created by this device.
  static constexpr uint8_t kPortId = 1;
  // Specifies the maximum transfer unit we support.
  // Picked to mimic common default ethernet frame size.
  static constexpr size_t kMtu = 1514;
  static constexpr size_t kFrameSize = sizeof(virtio_net_hdr_t) + kMtu;

  // Queue identifiers.
  static constexpr uint16_t kRxId = 0u;
  static constexpr uint16_t kTxId = 1u;

  NetworkDevice(fbl::RefPtr<BusTransactionInitiatorDispatcher> bti,
                ktl::unique_ptr<Backend> backend);
  virtual ~NetworkDevice();

  zx_status_t Init() override __TA_EXCLUDES(state_lock_);

  // VirtIO callbacks
  void IrqRingUpdate() override __TA_EXCLUDES(state_lock_);
  void IrqConfigChange() override __TA_EXCLUDES(state_lock_);

  // NetworkDeviceImpl protocol:
  void NetworkDeviceImplInit(const network_device_ifc_protocol_t* iface,
                             network_device_impl_init_callback callback, void* cookie);
  void NetworkDeviceImplStart(network_device_impl_start_callback callback, void* cookie);
  void NetworkDeviceImplStop(network_device_impl_stop_callback callback, void* cookie);
  void NetworkDeviceImplGetInfo(device_impl_info_t* out_info);
  void NetworkDeviceImplQueueTx(const tx_buffer_t* buf_list, size_t buf_count);
  void NetworkDeviceImplQueueRxSpace(const rx_space_buffer_t* buf_list, size_t buf_count);
  void NetworkDeviceImplPrepareVmo(uint8_t vmo_id, const uint8_t* vmo_list, size_t vmo_count,
                                   network_device_impl_prepare_vmo_callback callback, void* cookie);
  void NetworkDeviceImplReleaseVmo(uint8_t vmo_id);

  // NetworkPort protocol:
  void NetworkPortGetInfo(port_base_info_t* out_info);
  void NetworkPortGetStatus(port_status_t* out_status);
  void NetworkPortSetActive(bool active);
  void NetworkPortGetMac(mac_addr_protocol_t** out_mac_ifc);
  void NetworkPortRemoved() { /* do nothing, we never remove our port */ }

  // MacAddr protocol:
  void MacAddrGetAddress(mac_address_t* out_mac);
  void MacAddrGetFeatures(features_t* out_features);
  void MacAddrSetMode(mac_filter_mode_t mode, const mac_address_t* multicast_macs_list,
                      size_t multicast_macs_count);

  const char* tag() const override { return "virtio-net"; }

  uint16_t virtio_header_len() const { return virtio_hdr_len_; }

  network_device_impl_protocol_t device_impl_proto() {
    return network_device_impl_protocol_t{.ops = &network_device_impl_protocol_ops_, .ctx = this};
  }

 private:
  friend class NetworkDeviceTests;
  zx_status_t AckFeatures(bool* is_status_supported, bool* is_multiqueue_supported,
                          uint16_t* virtio_hdr_len);

  DISALLOW_COPY_ASSIGN_AND_MOVE(NetworkDevice);

  // Implementation of IrqRingUpdate; returns true if it should be called again.
  bool IrqRingUpdateInternal() __TA_EXCLUDES(state_lock_);
  port_status_t ReadStatus() const;

  // Mutexes to control concurrent access
  DECLARE_BRWLOCK_PI(NetworkDevice, lockdep::LockFlagsMultiAcquire) state_lock_;
  DECLARE_MUTEX(NetworkDevice) tx_lock_;
  DECLARE_MUTEX(NetworkDevice) rx_lock_;

  // Virtqueues; see section 5.1.2 of the spec
  //
  // This driver doesn't currently support multi-queueing, automatic
  // steering, or the control virtqueue, so only a single queue is needed in
  // each direction.
  //
  // https://docs.oasis-open.org/virtio/virtio/v1.1/csprd01/virtio-v1.1-csprd01.html#x1-1960002
  Ring rx_ __TA_GUARDED(rx_lock_);
  Ring tx_ __TA_GUARDED(tx_lock_);
  uint16_t rx_depth_;
  uint16_t tx_depth_;

  struct Descriptor {
    uint32_t buffer_id;
    uint16_t ring_id;
  };
  class FifoQueue {
   public:
    void Push(Descriptor t) {
      ZX_ASSERT(count_ < data_.size());
      data_[wr_] = t;
      wr_ = (wr_ + 1) % data_.size();
      count_++;
    }
    Descriptor Pop() {
      ZX_ASSERT(count_ > 0);
      Descriptor t = data_[rd_];
      rd_ = (rd_ + 1) % data_.size();
      count_--;
      return t;
    }
    bool Empty() const { return count_ == 0; }

   private:
    std::array<Descriptor, kMaxDepth> data_;
    size_t wr_ = 0;
    size_t rd_ = 0;
    size_t count_ = 0;
  };
  FifoQueue rx_in_flight_ __TA_GUARDED(rx_lock_);

  ktl::array<uint32_t, kMaxDepth> tx_in_flight_buffer_ids_ __TA_GUARDED(tx_lock_);
  std::bitset<kMaxDepth> tx_in_flight_active_ __TA_GUARDED(tx_lock_);

  // Whether the status field in virtio_net_config is supported.
  bool is_status_supported_;
  // Whether the device supports multiqueue with automatic receive steering.
  bool is_multiqueue_supported_;

  mac_address mac_;
  uint16_t virtio_hdr_len_;

  ddk::NetworkDeviceIfcProtocolClient ifc_ __TA_GUARDED(state_lock_);
  VmoStore vmo_store_ __TA_GUARDED(state_lock_);
  mac_addr_protocol_t mac_addr_proto_;
};

}  // namespace virtio

#endif  // SRC_CONNECTIVITY_ETHERNET_DRIVERS_VIRTIO_NETDEVICE_H_
