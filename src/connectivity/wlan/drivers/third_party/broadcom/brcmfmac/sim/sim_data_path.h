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

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_SIM_DATA_PATH_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_SIM_DATA_PATH_H_

#include <unordered_map>
#include <vector>

#include <wlan/common/macaddr.h>
#include <wlan/drivers/components/frame_storage.h>
#include <wlan/drivers/components/network_device.h>

namespace wlan::brcmfmac {

// Implements the tx/rx data path for the sim driver using the network device protocol,
// and manages the shared memory regions that are used by the protocol.
// Note that currently this class does not allow users to transmit or receive in batches.
// Frames must be transmitted one at a time.
class SimDataPath {
 public:
  // An arbitrarily chosen size for the maximum size of a frame that the sim driver can handle.
  static constexpr uint64_t kMaxFrameSize = 2048;

  SimDataPath() = default;

  // Initializes and starts the network device.
  void Init(drivers::components::NetworkDevice* net_dev);

  // Transmit a single ethernet frame. This will internally construct the ethernet header.
  // The `id` is used to identify the transmitted frame. Tests can keep track of the ids that are
  // sent and use it to verify that the expected frames were transmitted.
  void TxEthernet(uint16_t id, common::MacAddr dst, common::MacAddr src, uint16_t type,
                  cpp20::span<const uint8_t> body);

  // Transmit raw bytes contained in a body.
  void TxRaw(uint16_t id, const std::vector<uint8_t>& body);

  // Returns all the tx results received since this class was constructed.
  // Note that there is currently no way to clear the tx results during the lifetime of this class.
  const std::vector<tx_result_t>& TxResults() const { return tx_results_; }

  // Returns all the received frames since this class was constructed as vectors of bytes.
  // There is currently no way to clear the rx results during the lifetime of this class.
  const std::vector<std::vector<uint8_t>>& RxData() const { return rx_data_; }

  // networks_device_ifc_protocol_ops callbacks
  void OnTxComplete(const tx_result_t* tx_list, size_t tx_count);
  void OnRxComplete(const rx_buffer_t* rx_list, size_t rx_count);
  void OnAddPort(uint8_t id, const network_port_protocol_t* port);
  void OnRemovePort(uint8_t id);

 private:
  // Transmits a single frame that was written to the tx VMO starting at `offset` with the size
  // `data_size`.
  void TxRegion(uint16_t id, uint64_t offset, uint64_t data_size);

  // Queues a single buffer of size kMaxFrameSize to the network device to use for receiving data.
  void QueueRxBuffer();

  drivers::components::NetworkDevice* net_dev_ = nullptr;

  // A span of the VMO allocated for tx. Initialized on call to `Init()`.
  cpp20::span<uint8_t> tx_span_{};

  // A span of the VMO allocated for rx. Initialized on call to `Init()`.
  cpp20::span<uint8_t> rx_span_{};

  // Used for head/tail length requested by the device for transmits.
  device_impl_info_t device_info_{};

  // The list of tx results received during the lifetime of the class.
  // Populated during calls to OnTxComplete().
  std::vector<tx_result_t> tx_results_{};

  // The list of rx frames received during the lifetime of the class.
  // Populated during calls to OnRxComplete.
  std::vector<std::vector<uint8_t>> rx_data_;

  std::unordered_map<uint8_t, ddk::NetworkPortProtocolClient> port_clients_{};

  network_device_ifc_protocol_ops_t ifc_ops_ = {
      .add_port =
          [](void* ctx, uint8_t id, const network_port_protocol_t* port) {
            static_cast<SimDataPath*>(ctx)->OnAddPort(id, port);
          },
      .remove_port = [](void* ctx,
                        uint8_t id) { static_cast<SimDataPath*>(ctx)->OnRemovePort(id); },
      .complete_rx =
          [](void* ctx, const rx_buffer_t* rx_list, size_t rx_count) {
            static_cast<SimDataPath*>(ctx)->OnRxComplete(rx_list, rx_count);
          },
      .complete_tx =
          [](void* ctx, const tx_result_t* tx_list, size_t tx_count) {
            static_cast<SimDataPath*>(ctx)->OnTxComplete(tx_list, tx_count);
          },
  };

  network_device_ifc_protocol_t ifc_protocol_{.ops = &ifc_ops_, .ctx = this};
};

}  // namespace wlan::brcmfmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_SIM_DATA_PATH_H_
