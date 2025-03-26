// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_ETHERNET_DRIVERS_VIRTIO_NETDEVICE_H_
#define SRC_CONNECTIVITY_ETHERNET_DRIVERS_VIRTIO_NETDEVICE_H_

#include <fuchsia/hardware/network/driver/c/banjo.h>
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

#include <fbl/macros.h>
#include <kernel/brwlock.h>
#include <lwip/netif.h>
#include <virtio/net.h>

#include "src/connectivity/lib/network-device/buffer_descriptor/buffer_descriptor.h"
#include "src/connectivity/network/drivers/network-device/device/data_structs.h"
#include "src/lib/vmo_store/vmo_store.h"

namespace virtio {

using VmoStore = vmo_store::VmoStore<vmo_store::SlabStorage<uint32_t>>;

// Configuration for sessions opened by a `NetworkDeviceClient`.
struct SessionConfig {
  // Length of each buffer, in bytes.
  uint32_t buffer_length;
  // Buffer stride on VMO, in bytes.
  uint64_t buffer_stride;
  // Descriptor length, in bytes.
  uint64_t descriptor_length;
  // Tx header length, in bytes.
  uint16_t tx_header_length;
  // Tx tail length, in bytes.
  uint16_t tx_tail_length;
  // Number of rx descriptors to allocate.
  uint16_t rx_descriptor_count;
  // Number of tx descriptors to allocate.
  uint16_t tx_descriptor_count;
  // Session flags.
  // netdev::wire::SessionFlags options;

  // Verifies the session's configured parameters are appropriate for the constraints.
  zx_status_t Validate();
};

class NetworkDevice : public virtio::Device {
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
#ifndef __mist_os__
  // Picked to mimic common default ethernet frame size.
  static constexpr size_t kMtu = 1514;
#else
  // See https://cloud.google.com/compute/docs/troubleshooting/general-tips
  // See https://cloud.google.com/vpc/docs/mtu
  static constexpr size_t kMtu = 1460;
#endif
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
  void NetworkDeviceImplPrepareVmo(uint8_t vmo_id, fbl::RefPtr<VmObjectDispatcher> vmo,
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

  // LwIP integration:
  static err_t NetworkDeviceInit(struct netif*) TA_NO_THREAD_SAFETY_ANALYSIS;
  static err_t NetworkDeviceLinkOutput(struct netif* netif, struct pbuf* p);

  const char* tag() const override { return "virtio-net"; }

  uint16_t virtio_header_len() const { return virtio_hdr_len_; }

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

  VmoStore vmo_store_ __TA_GUARDED(state_lock_);
  mac_addr_protocol_t mac_addr_proto_;

  struct InFlightBuffer {
    InFlightBuffer() = default;
    InFlightBuffer(zx_status_t result, uint16_t descriptor_index)
        : result(result), descriptor_index(descriptor_index) {}
    // Session* session;
    zx_status_t result;
    uint16_t descriptor_index;
  };
  tx_buffer_t* GetBuffer();
  uint32_t Enqueue(uint16_t descriptor);
  void Push(uint16_t descriptor);
  void Commit();
  uint32_t tx_available() const { return tx_available_; }
  bool tx_overrun() const { return tx_available_ == 0; }
  uint32_t tx_available_ = 0;
  uint32_t tx_queued_ = 0;
  std::unique_ptr<network::internal::IndexedSlab<InFlightBuffer>>
      tx_in_flight_;  //__TA_GUARDED(tx_lock_);

  buffer_descriptor_t* descriptor(uint16_t idx);
  void* data(uint64_t offset);
  void ResetRxDescriptor(buffer_descriptor_t* descriptor);
  void ResetTxDescriptor(buffer_descriptor_t* descriptor);
  zx_status_t PrepareDescriptors();
  Buffer AllocTx();
  zx_status_t Send(Buffer* buffer);
  zx_status_t FetchTx();
  netif* ifc_ __TA_GUARDED(state_lock_);

  device_impl_info_t dev_info_;
  SessionConfig session_config_;
  uint16_t descriptor_count_;
  KernelHandle<VmObjectDispatcher> data_vmo_;
  fzl::VmoMapper data_;
  KernelHandle<VmObjectDispatcher> descriptors_vmo_;
  fzl::VmoMapper descriptors_;

  // rx descriptors ready to be sent back to the device.
  fbl::Vector<uint16_t> rx_out_queue_;
  // tx descriptors available to be written.
  // std::queue<uint16_t> tx_avail_;
  fbl::Vector<uint16_t> tx_avail_;
  // tx descriptors queued to be sent to device.
  fbl::Vector<uint16_t> tx_out_queue_;

  tx_buffer_t* tx_buffer_;
  buffer_region_t* tx_buffer_region_;

 public:
  // A contiguous buffer region.
  //
  // `Buffer`s are composed of N disjoint `BufferRegion`s, accessible through `BufferData`.
  class BufferRegion {
   public:
    // Caps this buffer to `len` bytes.
    //
    // If the buffer's length is smaller than `len`, `CapLength` does nothing.
    void CapLength(uint32_t len);
    uint32_t len() const;
    cpp20::span<uint8_t> data();
    cpp20::span<const uint8_t> data() const;
    // Writes `len` bytes from `src` into this region starting at `offset`.
    //
    // Returns the number of bytes that were written.
    size_t Write(const void* src, size_t len, size_t offset = 0);
    // Reads `len` bytes from this region into `dst` starting at `offset`.
    //
    // Returns the number of bytes that were read.
    size_t Read(void* dst, size_t len, size_t offset = 0) const;
    // Writes the `src` region starting at `src_offset` into this region starting at `offset`.
    //
    // Returns the number of bytes written.
    size_t Write(size_t offset, const BufferRegion& src, size_t src_offset);
    // Zero pads this region to `size`, returning the new size of the buffer.
    //
    // The returned value may be smaller than `size` if the amount of space available is not large
    // enough or larger if the buffer is already larger than `size.`
    size_t PadTo(size_t size);
    // Move assignment deleted to abide by `Buffer` destruction semantics.
    //
    // See `Buffer` for more details.
    BufferRegion& operator=(BufferRegion&&) = delete;
    BufferRegion(BufferRegion&& other) = default;

   protected:
    friend BufferData;
    BufferRegion() = default;

   private:
    void* base_;
    buffer_descriptor_t* desc_;

    DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(BufferRegion);
  };

  // A collection of `BufferRegion`s and metadata associated with a `Buffer`.
  class BufferData {
   public:
    // Returns true if this `BufferData` contains at least 1 `BufferRegion`.
    bool is_loaded() { return parts_count_ != 0; }
    // The number of `BufferRegion`s in this `BufferData`.
    uint32_t parts() { return parts_count_; }

    // Retrieves a `BufferRegion` part from this `BufferData`.
    //
    // Crashes if `idx >= parts()`.
    BufferRegion& part(size_t idx);
    const BufferRegion& part(size_t idx) const;
    // The total length, in bytes, of the buffer.
    uint32_t len() const;

    port_id_t port_id() const;
    frame_type_t frame_type() const;
    info_type_t info_type() const;
    uint32_t inbound_flags() const;
    uint32_t return_flags() const;

    void SetPortId(port_id_t port_id);
    void SetFrameType(frame_type_t type);
    void SetTxRequest(tx_flags_t tx_flags);

    // Writes up to `len` bytes from `src` into the buffer, returning the number of bytes written.
    size_t Write(const void* src, size_t len);
    // Reads up to `len` bytes from the buffer into `dst`, returning the number of bytes read.
    size_t Read(void* dst, size_t len) const;
    // Writes the contents of `data` into this buffer, returning the number of bytes written.
    size_t Write(const BufferData& data);
    // Zero pads buffer to total size `size`.
    //
    // Returns `ZX_ERR_BUFFER_TOO_SMALL` if buffer doesn't have enough available space to be padded
    // to size. No-op and returns `ZX_OK` if `len()` is at least `size`.
    zx_status_t PadTo(size_t size);

    // Move assignment deleted to abide by `Buffer` destruction semantics.
    //
    // See `Buffer` for more details.
    BufferData& operator=(BufferData&&) = delete;

   protected:
    friend Buffer;
    BufferData() = default;
    BufferData(BufferData&& other) = default;

    // Loads buffer information from `parent` using the descriptor `idx`.
    void Load(NetworkDevice* parent, uint16_t idx);

   private:
    uint32_t parts_count_ = 0;
    ktl::array<BufferRegion, MAX_DESCRIPTOR_CHAIN> parts_{};

    DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(BufferData);
  };

  class Buffer {
   public:
    Buffer();
    ~Buffer();
    Buffer(Buffer&& other) noexcept;
    // NOTE: A `Buffer` is returned to its parent on destruction. We'd have to do the same thing
    // with the target buffer on the move assignment operator, which can be very counter-intuitive.
    // We delete the move assignment operator to avoid confusion and misuse.
    Buffer& operator=(Buffer&&) = delete;

    bool is_valid() const { return parent_ != nullptr; }

    BufferData& data();
    const BufferData& data() const;

    // Equivalent to calling `Send(buffer)` on this buffer's `NetworkDeviceClient` parent.
    [[nodiscard]] zx_status_t Send();

   protected:
    friend NetworkDevice;
    Buffer(NetworkDevice* parent, uint16_t descriptor, bool rx);

   private:
    // Pointer device that owns this buffer, not owned.
    // A buffer with a null parent is an invalid buffer.
    NetworkDevice* parent_;
    uint16_t descriptor_;
    bool rx_;
    mutable BufferData data_;

    DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(Buffer);
  };
};

}  // namespace virtio

#endif  // SRC_CONNECTIVITY_ETHERNET_DRIVERS_VIRTIO_NETDEVICE_H_
