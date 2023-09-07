// Copyright (c) 2023 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim_data_path.h"

#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>

#include "fuchsia/hardware/network/driver/cpp/banjo.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim_utils.h"
#include "zircon/syscalls.h"

namespace wlan::brcmfmac {

namespace {

// NOTE: These can't conflict with the vmo id used internally by sim_fw.cc
constexpr uint8_t kTxVmoId = MAX_VMOS - 1;
constexpr uint8_t kRxVmoId = MAX_VMOS - 2;

zx::result<cpp20::span<uint8_t>> CreateAndMapVmo(zx::vmo& vmo, uint64_t req_size) {
  zx_status_t status = zx::vmo::create(req_size, 0, &vmo);
  if (status != ZX_OK) {
    return zx::error{status};
  }

  uint64_t size = 0;
  status = vmo.get_size(&size);
  if (status != ZX_OK) {
    return zx::error{status};
  }

  zx_vaddr_t addr = 0;
  status = zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0, size, &addr);
  if (status != ZX_OK) {
    return zx::error{status};
  }

  cpp20::span<uint8_t> ret = {reinterpret_cast<uint8_t*>(addr), size};
  return zx::success{ret};
}
}  // namespace

void SimDataPath::Init(drivers::components::NetworkDevice* net_dev) {
  ZX_ASSERT(net_dev);
  net_dev_ = net_dev;

  net_dev_->NetworkDeviceImplInit(&ifc_protocol_);
  net_dev_->NetworkDeviceImplGetInfo(&device_info_);

  // setup tx vmo, which is just a single frame of size kMaxFrameSize
  zx::vmo tx_vmo;
  auto tx_result = CreateAndMapVmo(tx_vmo, kMaxFrameSize);
  ZX_ASSERT(tx_result.is_ok());
  tx_span_ = tx_result.value();

  // setup rx vmo
  zx::vmo rx_vmo;
  auto rx_result = CreateAndMapVmo(rx_vmo, kMaxFrameSize);
  ZX_ASSERT(rx_result.is_ok());
  rx_span_ = rx_result.value();

  auto assert_ok = [](void* ctx, zx_status_t result) { ZX_ASSERT(result == ZX_OK); };

  // Give both vmos to net device
  net_dev_->NetworkDeviceImplPrepareVmo(kTxVmoId, std::move(tx_vmo), assert_ok, nullptr);
  net_dev_->NetworkDeviceImplPrepareVmo(kRxVmoId, std::move(rx_vmo), assert_ok, nullptr);

  net_dev_->NetworkDeviceImplStart(assert_ok, nullptr);

  // Give the rx buffer to the network device on init
  QueueRxBuffer();
}

void SimDataPath::TxEthernet(uint16_t id, common::MacAddr dst, common::MacAddr src, uint16_t type,
                             cpp20::span<const uint8_t> body) {
  ZX_ASSERT(net_dev_);
  ZX_ASSERT_MSG(body.size() + sim_utils::kEthernetHeaderSize + device_info_.tx_head_length +
                        device_info_.tx_tail_length <=
                    kMaxFrameSize,
                "Ethernet frame size too large");

  auto data_span =
      tx_span_.subspan(device_info_.tx_head_length, body.size() + sim_utils::kEthernetHeaderSize);
  ZX_ASSERT(sim_utils::WriteEthernetFrame(data_span, dst, src, type, body) == ZX_OK);

  TxRegion(id, 0, data_span.size());
}

void SimDataPath::TxRaw(uint16_t id, const std::vector<uint8_t>& body) {
  ZX_ASSERT(net_dev_);
  ZX_ASSERT_MSG(
      body.size() + device_info_.tx_head_length + device_info_.tx_tail_length <= kMaxFrameSize,
      "Frame size too large");

  auto data_span = tx_span_.subspan(device_info_.tx_head_length, body.size());
  memcpy(data_span.data(), body.data(), body.size());
  TxRegion(id, 0, data_span.size());
}

void SimDataPath::OnTxComplete(const tx_result_t* tx_list, size_t tx_count) {
  // Currently only one frame is sent at a time by the sim driver.
  // It is possible for this to be called with tx_count == 0 if frames owned internally by the
  // driver (not the network device) were transmitted.
  ZX_ASSERT(tx_count <= 1);

  if (tx_count) {
    tx_results_.push_back(tx_list[0]);
  }
}

void SimDataPath::OnRxComplete(const rx_buffer_t* rx_list, size_t rx_count) {
  ZX_ASSERT(rx_count <= 1);

  ZX_ASSERT(rx_list[0].data_count && rx_list[0].data_list);
  auto& region = *rx_list[0].data_list;

  ZX_ASSERT(region.offset + region.length <= rx_span_.size());

  // Copy rx data out so that it can be verified by the test.
  rx_data_.emplace_back();
  rx_data_.back().resize(region.length);
  memcpy(rx_data_.back().data(), rx_span_.subspan(region.offset, region.length).data(),
         region.length);

  // Immediately return the rx buffer to network device so users can rx more data
  QueueRxBuffer();
}

zx_status_t SimDataPath::OnAddPort(uint8_t id, const network_port_protocol_t* port) {
  ZX_ASSERT(port && port->ctx && port->ops);
  ZX_ASSERT(port_clients_.find(id) == port_clients_.end());
  port_clients_.insert({id, ddk::NetworkPortProtocolClient(port)});
  return ZX_OK;
}

void SimDataPath::OnRemovePort(uint8_t id) {
  auto client = port_clients_.find(id);
  ZX_ASSERT(client != port_clients_.end());
  client->second.Removed();
  port_clients_.erase(client);
}

void SimDataPath::TxRegion(uint16_t id, uint64_t offset, uint64_t data_size) {
  buffer_region_t region = {
      .vmo = kTxVmoId,
      .offset = offset,
      .length = data_size + device_info_.tx_head_length + device_info_.tx_tail_length,
  };

  tx_buffer_t buffer = {
      .id = id,
      .data_list = &region,
      .data_count = 1,
      .meta = {},
      .head_length = device_info_.tx_head_length,
      .tail_length = device_info_.tx_tail_length,
  };

  net_dev_->NetworkDeviceImplQueueTx(&buffer, 1);
}

void SimDataPath::QueueRxBuffer() {
  rx_space_buffer_t buffer = {.id = 0,
                              .region = {
                                  .vmo = kRxVmoId,
                                  .offset = 0,
                                  .length = kMaxFrameSize,
                              }};

  net_dev_->NetworkDeviceImplQueueRxSpace(&buffer, 1);
}

}  // namespace wlan::brcmfmac
