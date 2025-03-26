// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "netdevice.h"

#include <lib/fit/defer.h>
#include <lib/virtio/ring.h>
#include <lib/zircon-internal/align.h>
#include <limits.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <trace.h>
#include <zircon/assert.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <algorithm>

#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>
#include <lwip/netif.h>
#include <lwip/tcpip.h>
#include <netif/etharp.h>
#include <virtio/net.h>
#include <virtio/virtio.h>

#define LOCAL_TRACE 2

namespace virtio {

namespace {

bool IsLinkActive(const virtio_net_config& config, bool is_status_supported) {
  // 5.1.4.2 Driver Requirements: Device configuration layout
  //
  // If the driver does not negotiate the VIRTIO_NET_F_STATUS feature, it SHOULD assume the link
  // is active, otherwise it SHOULD read the link status from the bottom bit of status.
  //
  // https://docs.oasis-open.org/virtio/virtio/v1.1/csprd01/virtio-v1.1-csprd01.html#x1-2000004
  return is_status_supported ? config.status & VIRTIO_NET_S_LINK_UP : true;
}

uint16_t MaxVirtqueuePairs(const virtio_net_config& config, bool is_mq_supported) {
  // 5.1.5 Device Initialization
  //
  // Identify and initialize the receive and transmission virtqueues, up to N of each kind. If
  // VIRTIO_NET_F_MQ feature bit is negotiated, N=max_virtqueue_pairs, otherwise identify N=1.
  //
  // https://docs.oasis-open.org/virtio/virtio/v1.1/csprd01/virtio-v1.1-csprd01.html#x1-2040005
  return is_mq_supported ? config.max_virtqueue_pairs : 1;
}

}  // namespace

zx_status_t SessionConfig::Validate() {
  if (buffer_length <= tx_header_length + tx_tail_length) {
    LTRACEF("Invalid buffer length (%u), too small for requested Tx tail: (%u) + head: (%u)\n",
            buffer_length, tx_tail_length, tx_header_length);
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

NetworkDevice::NetworkDevice(fbl::RefPtr<BusTransactionInitiatorDispatcher> bti,
                             ktl::unique_ptr<Backend> backend)
    : virtio::Device(bti, ktl::move(backend)),
      rx_(this),
      tx_(this),
      vmo_store_({
          .map =
              vmo_store::MapOptions{
                  .vm_option = ZX_VM_PERM_READ | ZX_VM_PERM_WRITE | ZX_VM_REQUIRE_NON_RESIZABLE,
                  .vmar = nullptr,
              },
          .pin =
              vmo_store::PinOptions{
                  .bti = bti,
                  .bti_pin_options = ZX_BTI_PERM_READ | ZX_BTI_PERM_WRITE,
                  .index = true,
              },
      }) {}

NetworkDevice::~NetworkDevice() {}

zx_status_t NetworkDevice::Init() {
  Guard<BrwLockPi, BrwLockPi::Writer> guard(&state_lock_);

  // Reset the device.
  DeviceReset();

  // Ack and set the driver status bit.
  DriverStatusAck();

  // Ack features. We do DeviceStatusFeaturesOk() when we actually start the network device (in
  // NetworkDeviceImplStart()).
  if (zx_status_t status =
          AckFeatures(&is_status_supported_, &is_multiqueue_supported_, &virtio_hdr_len_);
      status != ZX_OK) {
    LTRACEF("failed to ack features: %d\n", status);
    return status;
  }

  // Read device configuration.
  virtio_net_config_t config;
  CopyDeviceConfig(&config, sizeof(config));

  // We've checked that the config.mac field is valid (VIRTIO_NET_F_MAC) in AckFeatures().
  LTRACEF("mac: %02x:%02x:%02x:%02x:%02x:%02x\n", config.mac[0], config.mac[1], config.mac[2],
          config.mac[3], config.mac[4], config.mac[5]);
  LTRACEF("link active: %u\n", IsLinkActive(config, is_status_supported_));
  LTRACEF("max virtqueue pairs: %u\n", MaxVirtqueuePairs(config, is_multiqueue_supported_));

  static_assert(sizeof(config.mac) == sizeof(mac_.octets));
  std::ranges::copy(config.mac, std::begin(mac_.octets));

  tx_depth_ = ktl::min(GetRingSize(kTxId), kMaxDepth);
  rx_depth_ = ktl::min(GetRingSize(kRxId), kMaxDepth);

  // Start the interrupt thread.
  StartIrqThread();

  // Add device to lwIP.
  fbl::AllocChecker ac;
  struct netif* netif = new (&ac) struct netif;
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  ifc_ = netif_add(netif, nullptr, nullptr, nullptr, this, NetworkDevice::NetworkDeviceInit,
                   tcpip_input);

  return ZX_OK;
}

zx_status_t NetworkDevice::AckFeatures(bool* is_status_supported, bool* is_multiqueue_supported,
                                       uint16_t* virtio_hdr_len) {
  const uint64_t supported_features = DeviceFeaturesSupported();

  if (!(supported_features & VIRTIO_NET_F_MAC)) {
    LTRACEF("device does not have a given MAC address.\n");
    return ZX_ERR_NOT_SUPPORTED;
  }
  uint64_t enable_features = VIRTIO_NET_F_MAC;

  if (supported_features & VIRTIO_NET_F_STATUS) {
    enable_features |= VIRTIO_NET_F_STATUS;
    *is_status_supported = true;
  } else {
    *is_status_supported = false;
  }

  if (supported_features & VIRTIO_NET_F_MQ) {
    enable_features |= VIRTIO_NET_F_MQ;
    *is_multiqueue_supported = true;
  } else {
    *is_multiqueue_supported = false;
  }

  if (supported_features & VIRTIO_F_VERSION_1) {
    enable_features |= VIRTIO_F_VERSION_1;
    *virtio_hdr_len = sizeof(virtio_net_hdr_t);
  } else {
    // 5.1.6.1 Legacy Interface: Device Operation.
    //
    // The legacy driver only presented num_buffers in the struct
    // virtio_net_hdr when VIRTIO_NET_F_MRG_RXBUF was negotiated; without
    // that feature the structure was 2 bytes shorter.
    //
    // https://docs.oasis-open.org/virtio/virtio/v1.1/csprd01/virtio-v1.1-csprd01.html#x1-2050006
    *virtio_hdr_len = sizeof(virtio_legacy_net_hdr_t);
  }

  DriverFeaturesAck(enable_features);
  return ZX_OK;
}

void NetworkDevice::IrqRingUpdate() {
  for (;;) {
    bool again = IrqRingUpdateInternal();
    if (!again) {
      break;
    }
  }
}

bool NetworkDevice::IrqRingUpdateInternal() {
  Guard<BrwLockPi, BrwLockPi::Reader> state_guard(&state_lock_);
  if (!ifc_) {
    return false;
  }

  bool more_work = false;

  ktl::array<tx_result_t, kMaxDepth> tx_results;
  auto tx_it = tx_results.begin();
  {
    Guard<Mutex> guard(&tx_lock_);
    tx_.SetNoInterrupt();
    // Ring::IrqRingUpdate will call this lambda on each tx buffer completed
    // by the underlying device since the last IRQ.
    tx_.IrqRingUpdate([this, &tx_it](vring_used_elem* used_elem) {
      []() __TA_ASSERT(tx_lock_) {}();
      uint16_t id = static_cast<uint16_t>(used_elem->id & 0xffff);
      ZX_ASSERT_MSG(id < kMaxDepth && tx_in_flight_active_[id], "%d is not active", id);
      *tx_it++ = {.id = tx_in_flight_buffer_ids_[id], .status = ZX_OK};
      tx_in_flight_active_[id] = false;
      tx_.FreeDesc(id);
    });
    more_work |= tx_.ClearNoInterruptCheckHasWork();
  }
  if (size_t count = std::distance(tx_results.begin(), tx_it); count != 0) {
    // ifc_.CompleteTx(tx_results.data(), count);
  }

  ktl::array<rx_buffer_t, kMaxDepth> rx_buffers;
  ktl::array<rx_buffer_part_t, kMaxDepth> rx_buffers_parts;
  auto rx_part_it = rx_buffers_parts.begin();
  auto rx_it = rx_buffers.begin();
  {
    Guard<Mutex> guard(&rx_lock_);
    rx_.SetNoInterrupt();
    // Ring::IrqRingUpdate will call this lambda on each rx buffer filled by
    // the underlying device since the last IRQ.
    rx_.IrqRingUpdate([this, &rx_it, &rx_part_it](vring_used_elem* used_elem) {
      []() __TA_ASSERT(rx_lock_) {}();
      uint16_t id = static_cast<uint16_t>(used_elem->id & 0xffff);
      Descriptor in_flight = rx_in_flight_.Pop();
      ZX_ASSERT_MSG(in_flight.ring_id == id,
                    "rx ring and FIFO id mismatch (%d != %d for buffer %d)", in_flight.ring_id, id,
                    in_flight.buffer_id);
      vring_desc& desc = *rx_.DescFromIndex(id);
      // Driver does not merge rx buffers.
      ZX_ASSERT_MSG((desc.flags & VRING_DESC_F_NEXT) == 0, "descriptor chaining not supported");

      auto parts_list = rx_part_it;
      uint32_t len = used_elem->len - virtio_hdr_len_;
      ZX_ASSERT_MSG(used_elem->len >= virtio_hdr_len_,
                    "got buffer (%u) smaller than virtio header (%u)", used_elem->len,
                    virtio_hdr_len_);
      LTRACEF("Receiving %d bytes (hdrlen = %u):\n", len, virtio_hdr_len_);
      if (LOCAL_TRACE >= 2) {
        virtio_dump_desc(&desc);
      }
      *rx_part_it++ = {
          .id = in_flight.buffer_id,
          .offset = virtio_hdr_len_,
          .length = len,
      };
      *rx_it++ = rx_buffer_t{
          .meta =
              {
                  .port = kPortId,
                  .frame_type = FRAME_TYPE_ETHERNET,
              },
          .data_list = &*parts_list,
          .data_count = 1,
      };
      rx_.FreeDesc(id);
    });
    more_work |= rx_.ClearNoInterruptCheckHasWork();
  }
  if (size_t count = std::distance(rx_buffers.begin(), rx_it); count != 0) {
    // ifc_.CompleteRx(rx_buffers.data(), count);
  }

  return more_work;
}

void NetworkDevice::IrqConfigChange() {
  Guard<BrwLockPi, BrwLockPi::Reader> state_guard(&state_lock_);

  const port_status_t status = ReadStatus();
  if (status.flags & STATUS_FLAGS_ONLINE) {
    printf("link up\n");
    netif_set_link_up(ifc_);
  } else {
    printf("link down\n");
    netif_set_link_down(ifc_);
  }
}

port_status_t NetworkDevice::ReadStatus() const {
  virtio_net_config config;
  CopyDeviceConfig(&config, sizeof(config));
  return {
      .flags = IsLinkActive(config, is_status_supported_) ? STATUS_FLAGS_ONLINE : 0,
      .mtu = kMtu,
  };
}

void NetworkDevice::NetworkDeviceImplInit(const network_device_ifc_protocol_t* iface,
                                          network_device_impl_init_callback callback,
                                          void* cookie) {
#if 0
  fbl::AutoLock lock(&state_lock_);
  ifc_ = ddk::NetworkDeviceIfcProtocolClient(iface);
  using Context = std::tuple<network_device_impl_init_callback, void*>;
  std::unique_ptr context = std::make_unique<Context>(callback, cookie);

  ifc_.AddPort(
      kPortId, this, &network_port_protocol_ops_,
      [](void* ctx, zx_status_t status) {
        std::unique_ptr<Context> context(static_cast<Context*>(ctx));
        auto [callback, cookie] = *context;
        if (status != ZX_OK) {
          zxlogf(ERROR, "failed to add port: %s", zx_status_get_string(status));
        }
        callback(cookie, status);
      },
      context.release());
#endif
}

void NetworkDevice::NetworkDeviceImplStart(network_device_impl_start_callback callback,
                                           void* cookie) {
  zx_status_t status = [this]() {
    // Always reset the device and reconfigure so we know where we are.
    DeviceReset();
    WaitForDeviceReset();
    DriverStatusAck();
    bool is_status_supported, is_multiqueue_supported;
    uint16_t header_length;
    if (zx_status_t status =
            AckFeatures(&is_status_supported, &is_multiqueue_supported, &header_length);
        status != ZX_OK) {
      LTRACEF("failed to ack features: %d\n", status);
      return status;
    }
    ZX_ASSERT_MSG(is_status_supported == is_status_supported_,
                  "status support changed from %u to %u between init and start",
                  is_status_supported_, is_status_supported);
    ZX_ASSERT_MSG(is_multiqueue_supported == is_multiqueue_supported_,
                  "max queue support changed from %u to %u between init and start",
                  is_multiqueue_supported_, is_multiqueue_supported);
    ZX_ASSERT_MSG(header_length == virtio_hdr_len_,
                  "header length changed from %u to %u between init and start", virtio_hdr_len_,
                  header_length);

    if (zx_status_t status = DeviceStatusFeaturesOk(); status != ZX_OK) {
      LTRACEF("%s: Feature negotiation failed (%s)\n", tag(), zx_status_get_string(status));
      return status;
    }

    // Allocate virtqueues.
    {
      Guard<Mutex> rx_guard(&rx_lock_);
      Guard<Mutex> tx_guard(&tx_lock_);
      Ring rx_queue(this);
      if (zx_status_t status = rx_queue.Init(kRxId, rx_depth_); status != ZX_OK) {
        LTRACEF("failed to allocate rx virtqueue: %d\n", status);
        return status;
      }
      rx_ = std::move(rx_queue);
      Ring tx_queue(this);
      if (zx_status_t status = tx_queue.Init(kTxId, tx_depth_); status != ZX_OK) {
        LTRACEF("failed to allocate tx virtqueue: %d\n", status);
        return status;
      }
      tx_ = std::move(tx_queue);
    }
    DriverStatusOk();
    return ZX_OK;
  }();
  callback(cookie, status);
}

void NetworkDevice::NetworkDeviceImplStop(network_device_impl_stop_callback callback,
                                          void* cookie) {
  DeviceReset();
  WaitForDeviceReset();

  // Return all pending buffers.
  {
    Guard<BrwLockPi, BrwLockPi::Reader> state_guard(&state_lock_);
    // Pending tx buffers.
    {
      std::array<tx_result_t, kMaxDepth> tx_return;
      auto iter = tx_return.begin();
      {
        Guard<Mutex> guard(&tx_lock_);
        // Free all TX ring entries to prevent the IRQ handler from completing these buffers.
        tx_.IrqRingUpdate([this](vring_used_elem* used_elem) {
          []() __TA_ASSERT(tx_lock_) {}();
          const uint16_t id = static_cast<uint16_t>(used_elem->id & 0xffff);
          tx_.FreeDesc(id);
        });

        for (int i = 0; i < kMaxDepth; ++i) {
          if (tx_in_flight_active_[i]) {
            *iter++ = {
                .id = tx_in_flight_buffer_ids_[i],
                .status = ZX_ERR_BAD_STATE,
            };
            tx_in_flight_active_[i] = false;
          }
        }
      }
      if (iter != tx_return.begin()) {
        // ifc_.CompleteTx(tx_return.data(), std::distance(tx_return.begin(), iter));
      }
    }
    // Pending rx buffers.
    {
      std::array<rx_buffer_t, kMaxDepth> rx_return;
      std::array<rx_buffer_part_t, kMaxDepth> rx_return_parts;
      auto iter = rx_return.begin();
      auto parts_iter = rx_return_parts.begin();
      {
        Guard<Mutex> guard(&rx_lock_);
        // Free all RX ring entries to prevent the IRQ handler from completing these buffers.
        rx_.IrqRingUpdate([this](vring_used_elem* used_elem) {
          []() __TA_ASSERT(rx_lock_) {}();
          const uint16_t id = static_cast<uint16_t>(used_elem->id & 0xffff);
          rx_.FreeDesc(id);
        });

        while (!rx_in_flight_.Empty()) {
          Descriptor d = rx_in_flight_.Pop();
          *iter++ = {
              .meta = {.frame_type = FRAME_TYPE_ETHERNET},
              .data_list = &*parts_iter,
              .data_count = 1,
          };
          *parts_iter++ = {.id = d.buffer_id};
        }
      }
      if (iter != rx_return.begin()) {
        // ifc_.CompleteRx(rx_return.data(), std::distance(rx_return.begin(), iter));
      }
    }
  }

  callback(cookie);
}

void NetworkDevice::NetworkDeviceImplGetInfo(device_impl_info_t* out_info) {
  *out_info = {
      .tx_depth = tx_depth_,
      .rx_depth = rx_depth_,
      .rx_threshold = static_cast<uint16_t>(rx_depth_ / 2),
      .max_buffer_parts = 1,
      .max_buffer_length = kFrameSize,
      .buffer_alignment = ZX_PAGE_SIZE / 2,
      .min_rx_buffer_length = kFrameSize,
      // Minimum Ethernet frame size on the wire according to IEEE 802.3, minus
      // the frame check sequence.
      .min_tx_buffer_length = 60,
      .tx_head_length = virtio_hdr_len_,
  };
}

void NetworkDevice::NetworkDeviceImplQueueTx(const tx_buffer_t* buf_list, size_t buf_count) {
  Guard<BrwLockPi, BrwLockPi::Reader> state_guard(&state_lock_);
  Guard<Mutex> guard(&tx_lock_);
  for (const auto& buffer : cpp20::span(buf_list, buf_count)) {
    ZX_DEBUG_ASSERT_MSG(buffer.data_count == 1, "received unsupported scatter gather buffer %zu",
                        buffer.data_count);

    const buffer_region_t& data = buffer.data_list[0];

    // Grab a free descriptor.
    uint16_t id;
    vring_desc* desc = tx_.AllocDescChain(1, &id);
    ZX_ASSERT_MSG(desc != nullptr, "failed to allocate descriptor");

    // Add the data to be sent.
    VmoStore::StoredVmo* stored_vmo = vmo_store_.GetVmo(data.vmo);
    ZX_ASSERT_MSG(stored_vmo != nullptr, "invalid VMO id %d", data.vmo);

    // Get a pointer to the header. Casting it to net header structs is valid
    // because we requested alignment and tx header in
    // NetworkDeviceImpl.GetInfo.
    void* tx_hdr = stored_vmo->data().subspan(data.offset, virtio_hdr_len_).data();

    constexpr virtio_legacy_net_hdr_t kBaseHeader = {
        // If VIRTIO_NET_F_CSUM is not negotiated, the driver MUST set flags to
        // zero and SHOULD supply a fully checksummed packet to the device.
        .flags = 0,
        // If none of the VIRTIO_NET_F_HOST_TSO4, TSO6 or UFO options have been
        // negotiated, the driver MUST set gso_type to VIRTIO_NET_HDR_GSO_NONE.
        .gso_type = VIRTIO_NET_HDR_GSO_NONE,
    };

    switch (virtio_hdr_len_) {
      case sizeof(virtio_net_hdr_t):
        *static_cast<virtio_net_hdr_t*>(tx_hdr) = {
            .base = kBaseHeader,
            // 5.1.6.2.1 Driver Requirements: Packet Transmission
            //
            // The driver MUST set num_buffers to zero.
            //
            // Implementation note: This field doesn't exist if neither
            // |VIRTIO_F_VERSION_1| or |VIRTIO_F_MRG_RXBUF| have been negotiated.
            //
            // https://docs.oasis-open.org/virtio/virtio/v1.1/csprd01/virtio-v1.1-csprd01.html#x1-2050006
            .num_buffers = 0,
        };
        break;
      case sizeof(virtio_legacy_net_hdr_t):
        *static_cast<virtio_legacy_net_hdr_t*>(tx_hdr) = kBaseHeader;
        break;
      default:
        ZX_PANIC("invalid virtio header length %d", virtio_hdr_len_);
    }

    fzl::PinnedVmo::Region region;
    size_t actual_regions = 0;
    zx_status_t status =
        stored_vmo->GetPinnedRegions(data.offset, data.length, &region, 1, &actual_regions);
    ZX_ASSERT_MSG(status == ZX_OK, "failed to retrieve pinned region %s (actual=%zu)",
                  zx_status_get_string(status), actual_regions);

    *desc = {
        .addr = region.phys_addr,
        .len = static_cast<uint32_t>(data.length),
    };
    tx_in_flight_buffer_ids_[id] = buffer.id;
    tx_in_flight_active_[id] = true;
    // Submit the descriptor and notify the back-end.
    if (LOCAL_TRACE >= 2) {
      virtio_dump_desc(desc);
    }
    LTRACEF("Sending %zu bytes (hdrlen = %u):\n", data.length, virtio_hdr_len_);
    tx_.SubmitChain(id);
  }
  if (!tx_.NoNotify()) {
    tx_.Kick();
  }
}

void NetworkDevice::NetworkDeviceImplQueueRxSpace(const rx_space_buffer_t* buf_list,
                                                  size_t buf_count) {
  Guard<BrwLockPi, BrwLockPi::Reader> state_guard(&state_lock_);
  Guard<Mutex> guard(&rx_lock_);
  for (const auto& buffer : cpp20::span(buf_list, buf_count)) {
    const buffer_region_t& data = buffer.region;

    // Grab a free descriptor.
    uint16_t id;
    vring_desc* desc = rx_.AllocDescChain(1, &id);
    ZX_ASSERT_MSG(desc != nullptr, "failed to allocate descriptor");

    // Add the data to be sent.
    VmoStore::StoredVmo* stored_vmo = vmo_store_.GetVmo(data.vmo);
    ZX_ASSERT_MSG(stored_vmo != nullptr, "invalid VMO id %d", data.vmo);

    fzl::PinnedVmo::Region region;
    size_t actual_regions = 0;
    zx_status_t status =
        stored_vmo->GetPinnedRegions(data.offset, data.length, &region, 1, &actual_regions);
    ZX_ASSERT_MSG(status == ZX_OK, "failed to retrieve pinned region %s (actual=%zu)",
                  zx_status_get_string(status), actual_regions);
    *desc = {
        .addr = region.phys_addr,
        .len = static_cast<uint32_t>(data.length),
        .flags = VRING_DESC_F_WRITE,
    };

    rx_in_flight_.Push({
        .buffer_id = buffer.id,
        .ring_id = id,
    });
    // Submit the descriptor and notify the back-end.
    if (LOCAL_TRACE >= 2) {
      virtio_dump_desc(desc);
    }
    LTRACEF("Queueing rx space with %zu bytes:\n", data.length);
    rx_.SubmitChain(id);
  }
  if (!rx_.NoNotify()) {
    rx_.Kick();
  }
}

void NetworkDevice::NetworkDeviceImplPrepareVmo(uint8_t vmo_id, fbl::RefPtr<VmObjectDispatcher> vmo,
                                                network_device_impl_prepare_vmo_callback callback,
                                                void* cookie) {
  zx_status_t status = [this, &vmo_id, &vmo]() {
    Guard<BrwLockPi, BrwLockPi::Writer> state_guard(&state_lock_);
    return vmo_store_.RegisterWithKey(vmo_id, ktl::move(vmo));
  }();
  callback(cookie, status);
}

void NetworkDevice::NetworkDeviceImplReleaseVmo(uint8_t vmo_id) {
  Guard<BrwLockPi, BrwLockPi::Writer> state_guard(&state_lock_);
  if (zx::result<fbl::RefPtr<VmObjectDispatcher>> status = vmo_store_.Unregister(vmo_id);
      status.status_value() != ZX_OK) {
    LTRACEF("failed to release vmo id = %d: %d\n", vmo_id, status.status_value());
  }
}

void NetworkDevice::NetworkPortGetInfo(port_base_info_t* out_info) {
  static constexpr uint8_t kRxTypesList[] = {static_cast<uint8_t>(FRAME_TYPE_ETHERNET)};
  static constexpr frame_type_support_t kTxTypesList[] = {{
      .type = static_cast<uint8_t>(FRAME_TYPE_ETHERNET),
      .features = FRAME_FEATURES_RAW,
  }};
  *out_info = {
      .port_class = PORT_CLASS_ETHERNET,
      .rx_types_list = kRxTypesList,
      .rx_types_count = std::size(kRxTypesList),
      .tx_types_list = kTxTypesList,
      .tx_types_count = std::size(kTxTypesList),
  };
}

void NetworkDevice::NetworkPortGetStatus(port_status_t* out_status) { *out_status = ReadStatus(); }

void NetworkDevice::NetworkPortSetActive(bool active) {}
void NetworkDevice::NetworkPortGetMac(mac_addr_protocol_t** out_mac_ifc) {
  if (out_mac_ifc) {
    *out_mac_ifc = &mac_addr_proto_;
  }
}

void NetworkDevice::MacAddrGetAddress(mac_address_t* out_mac) {
  std::ranges::copy(mac_.octets, out_mac->octets);
}

void NetworkDevice::MacAddrGetFeatures(features_t* out_features) {
  *out_features = {
      .multicast_filter_count = 0,
      .supported_modes = SUPPORTED_MAC_FILTER_MODE_PROMISCUOUS,
  };
}

void NetworkDevice::MacAddrSetMode(mac_filter_mode_t mode, const mac_address_t* multicast_macs_list,
                                   size_t multicast_macs_count) {
  /* We only support promiscuous mode, nothing to do */
  ZX_ASSERT_MSG(mode == MAC_FILTER_MODE_PROMISCUOUS, "unsupported mode %d", mode);
  ZX_ASSERT_MSG(multicast_macs_count == 0, "unsupported multicast count %zu", multicast_macs_count);
}

// --------------------------------------------------------------------------------------

err_t NetworkDevice::NetworkDeviceInit(struct netif* netif) {
  NetworkDevice* thiz = static_cast<NetworkDevice*>(netif->state);

  netif->name[0] = 'e';
  netif->name[1] = 'n';
  netif->hostname = nullptr;

  netif->output = etharp_output;
  netif->linkoutput = NetworkDevice::NetworkDeviceLinkOutput;
  // netif->output_ip6 = ethip6_output;

  static_assert(sizeof(netif->hwaddr) == sizeof(thiz->mac_.octets));
  std::ranges::copy(thiz->mac_.octets, std::begin(netif->hwaddr));
  netif->hwaddr_len = sizeof(thiz->mac_.octets);

  netif->flags = NETIF_FLAG_BROADCAST | NETIF_FLAG_ETHARP;
  const port_status_t port_status = thiz->ReadStatus();
  netif->mtu = static_cast<u16_t>(port_status.mtu);
  if (port_status.flags & STATUS_FLAGS_ONLINE) {
    netif->flags |= NETIF_FLAG_LINK_UP;
  } else {
    netif->flags &= ~NETIF_FLAG_LINK_UP;
  }

  std::optional<zx_status_t> start_result;
  thiz->NetworkDeviceImplStart(
      [](void* cookie, zx_status_t status) {
        *reinterpret_cast<std::optional<zx_status_t>*>(cookie) = status;
      },
      &start_result);
  ZX_ASSERT_MSG(start_result.value() == ZX_OK, "failed to start network device: %s",
                zx_status_get_string(start_result.value()));

  // The buffer length used by `DefaultSessionConfig`.
  static constexpr uint32_t kDefaultBufferLength = 2048;
  thiz->NetworkDeviceImplGetInfo(&thiz->dev_info_);
  const uint32_t buffer_length = std::min(kDefaultBufferLength, thiz->dev_info_.max_buffer_length);

  // This allows us to align up without a conditional, as explained here:
  // https://stackoverflow.com/a/9194117
  const uint64_t buffer_stride =
      ((buffer_length + thiz->dev_info_.buffer_alignment - 1) / thiz->dev_info_.buffer_alignment) *
      thiz->dev_info_.buffer_alignment;

  thiz->session_config_ = {
      .buffer_length = buffer_length,
      .buffer_stride = buffer_stride,
      .descriptor_length = sizeof(buffer_descriptor_t),
      .tx_header_length = static_cast<uint16_t>(thiz->dev_info_.min_tx_buffer_length),
      .tx_tail_length = static_cast<uint16_t>(thiz->dev_info_.min_tx_buffer_length),
      .rx_descriptor_count = thiz->dev_info_.rx_depth,
      .tx_descriptor_count = thiz->dev_info_.tx_depth,
      //.options = netdev::wire::SessionFlags::kPrimary,
  };

  if (thiz->session_config_.descriptor_length < sizeof(buffer_descriptor_t) ||
      (thiz->session_config_.descriptor_length % sizeof(uint64_t)) != 0) {
    LTRACEF("Invalid descriptor length %lu\n", thiz->session_config_.descriptor_length);
    return ZX_ERR_INVALID_ARGS;
  }

  if (thiz->session_config_.rx_descriptor_count > kMaxDepth ||
      thiz->session_config_.tx_descriptor_count > kMaxDepth) {
    LTRACEF(
        "Invalid descriptor count  %u/%u, this client supports a maximum depth of %u descriptors\n",
        thiz->session_config_.rx_descriptor_count, thiz->session_config_.tx_descriptor_count,
        kMaxDepth);
    return ZX_ERR_INVALID_ARGS;
  }

  if (thiz->session_config_.buffer_stride < thiz->session_config_.buffer_length) {
    LTRACEF("Stride in VMO can't be smaller than buffer length\n");
    return ZX_ERR_INVALID_ARGS;
  }

  if (thiz->session_config_.buffer_stride % thiz->dev_info_.buffer_alignment != 0) {
    LTRACEF("Buffer stride %lu does not meet buffer alignment requirement: %u\n",
            thiz->session_config_.buffer_stride, thiz->dev_info_.buffer_alignment);
    return ZX_ERR_INVALID_ARGS;
  }

  thiz->descriptor_count_ =
      thiz->session_config_.rx_descriptor_count + thiz->session_config_.tx_descriptor_count;
  // Check if sum of descriptor count overflows.
  if (thiz->descriptor_count_ < thiz->session_config_.rx_descriptor_count ||
      thiz->descriptor_count_ < thiz->session_config_.tx_descriptor_count) {
    LTRACEF("Invalid descriptor count, maximum total descriptors must be less than 2^16\n");
    return ZX_ERR_INVALID_ARGS;
  }

  if (thiz->session_config_.Validate() != ZX_OK) {
    return ZX_ERR_INVALID_ARGS;
  }

  uint64_t data_vmo_size = thiz->descriptor_count_ * thiz->session_config_.buffer_stride;
  printf("data_vmo_size: %lu\n", data_vmo_size);

  // Create a VMO for the data.
  fbl::RefPtr<VmObjectPaged> vmo;
  uint64_t aligned_size;
  zx_status_t status = VmObject::RoundSize(data_vmo_size, &aligned_size);
  ZX_ASSERT(status == ZX_OK);
  status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, aligned_size, &vmo);
  ZX_ASSERT(status == ZX_OK);

  // create a Vm Object dispatcher
  ZX_ASSERT(status == ZX_OK);
  if (status = thiz->data_.CreateAndMap(data_vmo_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr,
                                        &thiz->data_vmo_);
      status != ZX_OK) {
    LTRACEF("Failed to create data VMO: %s\n", zx_status_get_string(status));
    return ERR_ABRT;
  }

  if (status = thiz->vmo_store_.Reserve(MAX_VMOS); status != ZX_OK) {
    LTRACEF("init: failed to init session identifiers %d\n", status);
    return ERR_ABRT;
  }

  auto key = thiz->vmo_store_.Register(thiz->data_vmo_.dispatcher());
  if (key.is_error()) {
    LTRACEF("failed to register vmo: %d\n", key.error_value());
    return ERR_ABRT;
  }
  LTRACEF("key: %d\n", key.value());

#if 0
  thiz->NetworkDeviceImplPrepareVmo(
      static_cast<uint8_t>(key.value()), ktl::move(thiz->vmo_store_.GetVmo(key.value())->vmo()),
      [](void* cookie, zx_status_t status) {
        if (status != ZX_OK) {
          LTRACEF("failed to prepare vmo: %d\n", status);
        }
      },
      nullptr);
#endif

  uint64_t descriptors_vmo_size = thiz->descriptor_count_ * thiz->session_config_.descriptor_length;
  if (status =
          thiz->descriptors_.CreateAndMap(descriptors_vmo_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE,
                                          nullptr, &thiz->descriptors_vmo_);
      status != ZX_OK) {
    LTRACEF("Failed to create descriptors VMO: %s\n", zx_status_get_string(status));
    return ERR_ABRT;
  }

  fbl::AllocChecker ac;
  thiz->tx_buffer_ = new (&ac) tx_buffer_t[thiz->tx_depth_];
  ZX_ASSERT(ac.check());
  for (uint16_t i = 0; i < thiz->tx_depth_; i++) {
    memset(&thiz->tx_buffer_[i], 0, sizeof(tx_buffer_t));

    thiz->tx_buffer_[i].data_list = new (&ac) buffer_region_t[thiz->tx_depth_];
    ZX_ASSERT(ac.check());

    // memset(&thiz->tx_buffer_[i].data_list[0], 0, sizeof(buffer_region_t) * thiz->tx_depth_);
  }

  zx::result in_flight = network::internal::IndexedSlab<InFlightBuffer>::Create(thiz->tx_depth_);
  if (in_flight.is_error()) {
    return ERR_ABRT;
  }
  thiz->tx_in_flight_ = std::move(in_flight.value());
  thiz->tx_available_ = thiz->tx_in_flight_->available();

  thiz->PrepareDescriptors();

  return start_result.value() == ZX_OK ? ERR_OK : ERR_ABRT;
}

err_t NetworkDevice::NetworkDeviceLinkOutput(struct netif* netif, struct pbuf* p) {
  LTRACEF("netif %p buf %p\n", netif, p);

  NetworkDevice* thiz = static_cast<NetworkDevice*>(netif->state);

  Buffer buffer = thiz->AllocTx();
  if (!buffer.is_valid()) {
    return ERR_ABRT;
  }

  for (struct pbuf* q = p; q != nullptr; q = q->next) {
    LTRACEF("q->len: %d\n", q->len);
    buffer.data().Write(q->payload, q->len);
  }

  return buffer.Send() == ZX_OK ? ERR_OK : ERR_ABRT;
}

// --------------------------------------------------------------------------------------

uint32_t NetworkDevice::Enqueue(uint16_t descriptor) {
  return tx_in_flight_->Push(InFlightBuffer(ZX_OK, descriptor));
}

tx_buffer_t* NetworkDevice::GetBuffer() {
  ZX_ASSERT(tx_available_ != 0);
  return &tx_buffer_[tx_queued_];
}

void NetworkDevice::Push(uint16_t descriptor) {
  ZX_ASSERT(tx_available_ != 0);
  // session_->TxTaken();
  tx_buffer_[tx_queued_].id = Enqueue(descriptor);
  tx_available_--;
  tx_queued_++;
}

void NetworkDevice::Commit() {
  // when we destroy a session transaction, we commit all the queued buffers to
  // the device.
  if (tx_queued_ != 0) {
    // Send buffers in batches of at most |MAX_TX_BUFFERS| at a time to stay within the FIDL
    // channel maximum.
    tx_buffer_t* buffers = tx_buffer_;
    while (tx_queued_ > 0) {
      const uint32_t batch = std::min(static_cast<uint32_t>(tx_queued_), MAX_TX_BUFFERS);
      NetworkDeviceImplQueueTx(buffers, batch);
      buffers += batch;
      tx_queued_ -= batch;
    }
  }
}

// --------------------------------------------------------------------------------------

buffer_descriptor_t* NetworkDevice::descriptor(uint16_t idx) {
  ZX_ASSERT_MSG(idx < descriptor_count_, "invalid index %d, want < %d", idx, descriptor_count_);
  ZX_ASSERT_MSG(descriptors_.start() != nullptr, "descriptors not mapped");
  return reinterpret_cast<buffer_descriptor_t*>(static_cast<uint8_t*>(descriptors_.start()) +
                                                session_config_.descriptor_length * idx);
}

void* NetworkDevice::data(uint64_t offset) {
  ZX_ASSERT(offset < data_.size());
  return static_cast<uint8_t*>(data_.start()) + offset;
}

void NetworkDevice::ResetRxDescriptor(buffer_descriptor_t* descriptor) {
  *descriptor = {
      .nxt = 0xFFFF,
      .info_type = static_cast<uint32_t>(INFO_TYPE_NO_INFO),
      .offset = descriptor->offset,
      .data_length = session_config_.buffer_length,
  };
}

void NetworkDevice::ResetTxDescriptor(buffer_descriptor_t* descriptor) {
  *descriptor = {
      .nxt = 0xFFFF,
      .info_type = static_cast<uint32_t>(INFO_TYPE_NO_INFO),
      .offset = descriptor->offset,
      .head_length = session_config_.tx_header_length,
      .tail_length = session_config_.tx_tail_length,
      .data_length = session_config_.buffer_length - session_config_.tx_header_length -
                     session_config_.tx_tail_length,
  };
}

zx_status_t NetworkDevice::PrepareDescriptors() {
  fbl::AllocChecker ac;
  uint16_t desc = 0;
  uint64_t buff_off = 0;
  auto* pDesc = static_cast<uint8_t*>(descriptors_.start());

  rx_out_queue_.reserve(session_config_.rx_descriptor_count, &ac);
  ZX_ASSERT(ac.check());

  for (; desc < session_config_.rx_descriptor_count; desc++) {
    auto* descriptor = reinterpret_cast<buffer_descriptor_t*>(pDesc);
    descriptor->offset = buff_off;
    ResetRxDescriptor(descriptor);

    buff_off += session_config_.buffer_stride;
    pDesc += session_config_.descriptor_length;
    rx_out_queue_.push_back(desc, &ac);
    ZX_ASSERT(ac.check());
  }

  for (; desc < descriptor_count_; desc++) {
    auto* descriptor = reinterpret_cast<buffer_descriptor_t*>(pDesc);
    ResetTxDescriptor(descriptor);
    descriptor->offset = buff_off;

    buff_off += session_config_.buffer_stride;
    pDesc += session_config_.descriptor_length;
    tx_avail_.push_back(desc, &ac);
    ZX_ASSERT(ac.check());
  }

  return ZX_OK;
}

NetworkDevice::Buffer NetworkDevice::AllocTx() {
  if (tx_avail_.is_empty()) {
    return Buffer();
  } else {
    auto idx = tx_avail_.erase(0);
    printf("AllocTx: %d\n", idx);
    return Buffer(this, idx, false);
  }
}

zx_status_t NetworkDevice::FetchTx() {
  constexpr uint16_t kMaxFifoDepth = ZX_PAGE_SIZE / sizeof(uint16_t);
  uint16_t fetch_buffer[kMaxFifoDepth];
  auto read = tx_out_queue_.size();
  for (uint16_t i = 0; i < read; i++) {
    fetch_buffer[i] = tx_out_queue_.erase(0);
  }

  cpp20::span descriptors(fetch_buffer, read);

  int16_t req_header_length = session_config_.tx_header_length;
  uint16_t req_tail_length = session_config_.tx_tail_length;

  // SharedAutoLock lock(&parent_->control_lock());
  for (uint16_t desc_idx : descriptors) {
    buffer_descriptor_t* const desc_ptr = descriptor(desc_idx);
    if (!desc_ptr) {
      TRACEF("received out of bounds descriptor: %d\n", desc_idx);
      return ZX_ERR_IO_INVALID;
    }
    buffer_descriptor_t& desc = *desc_ptr;

    tx_buffer_t* buffer = GetBuffer();

    // check header space:
    if (desc.head_length < req_header_length) {
      TRACEF("received buffer with insufficient head length: %d\n", desc.head_length);
      return ZX_ERR_IO_INVALID;
    }
    auto skip_front = desc.head_length - req_header_length;

    // check tail space:
    if (desc.tail_length < req_tail_length) {
      TRACEF("received buffer with insufficient tail length: %d\n", desc.tail_length);
      return ZX_ERR_IO_INVALID;
    }

    auto info_type = (desc.info_type);
    switch (info_type) {
      case INFO_TYPE_NO_INFO:
        break;
      default:
        TRACEF("info type (%u) not recognized, discarding information\n", info_type);
        info_type = INFO_TYPE_NO_INFO;
        break;
    }

    buffer->data_count = 0;
    buffer->meta.port = desc.port_id.base;
    buffer->meta.flags = desc.inbound_flags;
    buffer->meta.frame_type = desc.frame_type;
    buffer->head_length = req_header_length;
    buffer->tail_length = req_tail_length;

    // chain_length is the number of buffers to follow, so it must be strictly less than the maximum
    // descriptor chain value.
    if (desc.chain_length >= MAX_DESCRIPTOR_CHAIN) {
      TRACEF("received invalid chain length: %d\n", desc.chain_length);
      return ZX_ERR_IO_INVALID;
    }
    auto expect_chain = desc.chain_length;

    bool add_head_space = buffer->head_length != 0;
    buffer_descriptor_t* part_iter = desc_ptr;
    uint32_t total_length = 0;
    for (;;) {
      buffer_descriptor_t& part_desc = *part_iter;
      auto* cur = const_cast<buffer_region_t*>(&buffer->data_list[buffer->data_count]);
      if (add_head_space) {
        // cur->vmo = vmo_id_;
        cur->offset = part_desc.offset + skip_front;
        cur->length = part_desc.data_length + buffer->head_length;
      } else {
        // cur->vmo = vmo_id_;
        cur->offset = part_desc.offset + part_desc.head_length;
        cur->length = part_desc.data_length;
      }
      if (expect_chain == 0 && buffer->tail_length) {
        cur->length += buffer->tail_length;
      }
      total_length += part_desc.data_length;
      buffer->data_count++;

      add_head_space = false;
      if (expect_chain == 0) {
        break;
      }
      uint16_t didx = part_desc.nxt;
      part_iter = descriptor(didx);
      if (part_iter == nullptr) {
        TRACEF("invalid chained descriptor index: %d\n", didx);
        return ZX_ERR_IO_INVALID;
      }
      buffer_descriptor_t& next_desc = *part_iter;
      if (next_desc.chain_length != expect_chain - 1) {
        TRACEF("invalid next chain length %d on descriptor %d\n", next_desc.chain_length, didx);
        return ZX_ERR_IO_INVALID;
      }
      expect_chain--;
    }

    if (total_length < dev_info_.min_tx_buffer_length) {
      TRACEF("tx buffer length %d less than required minimum of %d\n", total_length,
             dev_info_.min_tx_buffer_length);
      return ZX_ERR_IO_INVALID;
    }

    Push(desc_idx);
  }

  return tx_overrun() ? ZX_ERR_IO_OVERRUN : ZX_OK;
}

zx_status_t NetworkDevice::Send(NetworkDevice::Buffer* buffer) {
  if (!buffer->is_valid()) {
    return ZX_ERR_UNAVAILABLE;
  }
  if (buffer->rx_) {
    // If this is an RX buffer, we need to get a TX buffer from the pool and return it as an RX
    // buffer in place of this.
    auto tx_buffer = AllocTx();
    if (!tx_buffer.is_valid()) {
      return ZX_ERR_NO_RESOURCES;
    }
    // Flip the buffer, it'll be returned to the rx queue on destruction.
    tx_buffer.rx_ = true;
    buffer->rx_ = false;
  }
  // if (!tx_writable_wait_.is_pending()) {
  //   zx_status_t status = tx_writable_wait_.Begin(dispatcher_);
  //   if (status != ZX_OK) {
  //     return status;
  //   }
  // }
  fbl::AllocChecker ac;
  tx_out_queue_.push_back(buffer->descriptor_, &ac);
  ZX_ASSERT(ac.check());

  // Don't return this buffer on destruction.
  // Also invalidate it.
  buffer->parent_ = nullptr;

  zx_status_t status = FetchTx();
  switch (status) {
    case ZX_OK:
    case ZX_ERR_SHOULD_WAIT:
      break;
    case ZX_ERR_PEER_CLOSED:
      // FIFO is closed, don't reinstall the wait.
    case ZX_ERR_IO_OVERRUN:
      // We've run out of device buffers, don't reinstall the wait until we're
      // notified of new buffers.
      return ZX_OK;
    default:
      TRACEF("unexpected error: %s\n", zx_status_get_string(status));
      return ZX_OK;
  }
  Commit();
  return ZX_OK;
}

NetworkDevice::Buffer::Buffer() : parent_(nullptr), descriptor_(0), rx_(false) {}

NetworkDevice::Buffer::Buffer(NetworkDevice* parent, uint16_t descriptor, bool rx)
    : parent_(parent), descriptor_(descriptor), rx_(rx) {}

NetworkDevice::Buffer::Buffer(NetworkDevice::Buffer&& other) noexcept
    : parent_(other.parent_),
      descriptor_(other.descriptor_),
      rx_(other.rx_),
      data_(std::move(other.data_)) {
  other.parent_ = nullptr;
}

NetworkDevice::Buffer::~Buffer() {
  if (parent_) {
    if (rx_) {
      // parent_->ReturnRxDescriptor(descriptor_);
    } else {
      // parent_->ReturnTxDescriptor(descriptor_);
    }
  }
}

NetworkDevice::BufferData& NetworkDevice::Buffer::data() {
  ZX_ASSERT(is_valid());
  if (!data_.is_loaded()) {
    data_.Load(parent_, descriptor_);
  }
  return data_;
}

const NetworkDevice::BufferData& NetworkDevice::Buffer::data() const {
  ZX_ASSERT(is_valid());
  if (!data_.is_loaded()) {
    data_.Load(parent_, descriptor_);
  }
  return data_;
}

zx_status_t NetworkDevice::Buffer::Send() {
  if (!is_valid()) {
    return ZX_ERR_UNAVAILABLE;
  }
  zx_status_t status = data_.PadTo(parent_->dev_info_.min_tx_buffer_length);
  if (status != ZX_OK) {
    return status;
  }
  return parent_->Send(this);
}

void NetworkDevice::BufferData::Load(NetworkDevice* parent, uint16_t idx) {
  auto* desc = parent->descriptor(idx);
  while (desc) {
    auto& cur = parts_[parts_count_];
    cur.base_ = parent->data(desc->offset + desc->head_length);
    cur.desc_ = desc;
    parts_count_++;
    if (desc->chain_length != 0) {
      desc = parent->descriptor(desc->nxt);
    } else {
      desc = nullptr;
    }
  }
}

NetworkDevice::BufferRegion& NetworkDevice::BufferData::part(size_t idx) {
  ZX_ASSERT(idx < parts_count_);
  return parts_[idx];
}

const NetworkDevice::BufferRegion& NetworkDevice::BufferData::part(size_t idx) const {
  ZX_ASSERT(idx < parts_count_);
  return parts_[idx];
}

uint32_t NetworkDevice::BufferData::len() const {
  uint32_t c = 0;
  for (uint32_t i = 0; i < parts_count_; i++) {
    c += parts_[i].len();
  }
  return c;
}

frame_type_t NetworkDevice::BufferData::frame_type() const {
  return static_cast<frame_type_t>(part(0).desc_->frame_type);
}

void NetworkDevice::BufferData::SetFrameType(frame_type_t type) {
  part(0).desc_->frame_type = static_cast<uint8_t>(type);
}

port_id_t NetworkDevice::BufferData::port_id() const {
  const buffer_descriptor_t& desc = *part(0).desc_;
  return {
      .base = desc.port_id.base,
      .salt = desc.port_id.salt,
  };
}

void NetworkDevice::BufferData::SetPortId(port_id_t port_id) {
  buffer_descriptor_t& desc = *part(0).desc_;
  desc.port_id = {
      .base = port_id.base,
      .salt = port_id.salt,
  };
}

info_type_t NetworkDevice::BufferData::info_type() const {
  return static_cast<info_type_t>(part(0).desc_->frame_type);
}

uint32_t NetworkDevice::BufferData::inbound_flags() const { return part(0).desc_->inbound_flags; }

uint32_t NetworkDevice::BufferData::return_flags() const { return part(0).desc_->return_flags; }

void NetworkDevice::BufferData::SetTxRequest(tx_flags_t tx_flags) {
  part(0).desc_->inbound_flags = static_cast<uint32_t>(tx_flags);
}

size_t NetworkDevice::BufferData::Write(const void* src, size_t len) {
  const auto* ptr = static_cast<const uint8_t*>(src);
  size_t written = 0;
  for (uint32_t i = 0; i < parts_count_; i++) {
    auto& part = parts_[i];
    uint32_t wr = std::min(static_cast<uint32_t>(len - written), part.len());
    part.Write(ptr, wr);
    ptr += wr;
    written += wr;
  }
  return written;
}

size_t NetworkDevice::BufferData::Write(const BufferData& data) {
  size_t count = 0;

  size_t idx_me = 0;
  size_t offset_me = 0;
  size_t offset_other = 0;
  for (size_t idx_o = 0; idx_o < data.parts_count_ && idx_me < parts_count_;) {
    size_t wr = parts_[idx_me].Write(offset_me, data.parts_[idx_o], offset_other);
    offset_me += wr;
    offset_other += wr;
    count += wr;
    if (offset_me >= parts_[idx_me].len()) {
      idx_me++;
      offset_me = 0;
    }
    if (offset_other >= data.parts_[idx_o].len()) {
      idx_o++;
      offset_other = 0;
    }
  }
  // Update the length on the last descriptor.
  if (idx_me < parts_count_) {
    ZX_DEBUG_ASSERT(offset_me <= std::numeric_limits<uint32_t>::max());
    parts_[idx_me].CapLength(static_cast<uint32_t>(offset_me));
  }

  return count;
}

zx_status_t NetworkDevice::BufferData::PadTo(size_t size) {
  size_t total_size = 0;
  for (uint32_t i = 0; i < parts_count_ && total_size < size; i++) {
    total_size += parts_[i].PadTo(size - total_size);
  }
  if (total_size < size) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }
  return ZX_OK;
}

size_t NetworkDevice::BufferData::Read(void* dst, size_t len) const {
  auto* ptr = static_cast<uint8_t*>(dst);
  size_t actual = 0;
  for (uint32_t i = 0; i < parts_count_ && len > 0; i++) {
    auto& part = parts_[i];
    size_t rd = part.Read(ptr, len);
    len -= rd;
    ptr += rd;
    actual += rd;
  }
  return actual;
}

void NetworkDevice::BufferRegion::CapLength(uint32_t len) {
  if (len <= desc_->data_length) {
    desc_->tail_length += desc_->data_length - len;
    desc_->data_length = len;
  }
}

uint32_t NetworkDevice::BufferRegion::len() const { return desc_->data_length; }

cpp20::span<uint8_t> NetworkDevice::BufferRegion::data() {
  return cpp20::span(static_cast<uint8_t*>(base_), len());
}

cpp20::span<const uint8_t> NetworkDevice::BufferRegion::data() const {
  return cpp20::span(static_cast<const uint8_t*>(base_), len());
}

size_t NetworkDevice::BufferRegion::Write(const void* src, size_t len, size_t offset) {
  uint32_t nlen = std::min(desc_->data_length, static_cast<uint32_t>(len + offset));
  CapLength(nlen);
  std::copy_n(static_cast<const uint8_t*>(src), this->len() - offset, data().begin() + offset);
  return this->len();
}

size_t NetworkDevice::BufferRegion::Read(void* dst, size_t len, size_t offset) const {
  if (offset >= desc_->data_length) {
    return 0;
  }
  len = std::min(len, desc_->data_length - offset);
  std::copy_n(data().begin() + offset, len, static_cast<uint8_t*>(dst));
  return len;
}

size_t NetworkDevice::BufferRegion::Write(size_t offset, const BufferRegion& src,
                                          size_t src_offset) {
  if (offset >= desc_->data_length || src_offset >= src.desc_->data_length) {
    return 0;
  }
  size_t wr = std::min(desc_->data_length - offset, src.desc_->data_length - src_offset);
  std::copy_n(src.data().begin() + src_offset, wr, data().begin() + offset);
  return wr;
}

size_t NetworkDevice::BufferRegion::PadTo(size_t size) {
  if (size > desc_->data_length) {
    size -= desc_->data_length;
    cpp20::span<uint8_t> pad(static_cast<uint8_t*>(base_) + desc_->head_length + desc_->data_length,
                             std::min(size, static_cast<size_t>(desc_->tail_length)));
    memset(pad.data(), 0x00, pad.size());
    desc_->data_length += pad.size();
    desc_->tail_length -= pad.size();
  }
  return desc_->data_length;
}

}  // namespace virtio
