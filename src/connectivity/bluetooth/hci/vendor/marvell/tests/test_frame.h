// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_TEST_FRAME_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_TEST_FRAME_H_

#include <lib/fzl/vmo-mapper.h>

#include <fbl/algorithm.h>

#include "src/connectivity/bluetooth/hci/vendor/marvell/device_oracle.h"

namespace bt_hci_marvell {

// A class to generate test frames, both raw data (as seen by the host channels), and allocated
// in a vmo with a header (as seen by the SDIO bus). For now we only auto-generate the payload
// based on size, but we could easily add a new constructor to support caller-provided payload data.
class TestFrame {
 public:
  TestFrame() = delete;

  TestFrame(std::vector<uint8_t> data, ControllerChannelId channel_id,
            std::optional<DeviceOracle> device_oracle)
      : channel_id_(channel_id), data_(data), device_oracle_(device_oracle) {
    CreateSdioTxn();
  }

  TestFrame(uint32_t size, ControllerChannelId channel_id,
            std::optional<DeviceOracle> device_oracle)
      : channel_id_(channel_id), device_oracle_(device_oracle) {
    data_.resize(size);

    // Auto-generate the payload
    for (uint32_t ndx = 0; ndx < size; ndx++) {
      data_[ndx] = ExpectedPayload(ndx);
    }

    CreateSdioTxn();
  }

  void CreateSdioTxn() {
    // Create a vmo to hold the SDIO frame (that includes the header).
    uint32_t frame_size_with_header =
        static_cast<uint32_t>(data_.size() + MarvellFrameHeaderView::SizeInBytes());
    uint32_t buffer_size = fbl::round_up<uint32_t, uint32_t>(frame_size_with_header,
                                                             device_oracle_->GetSdioBlockSize());
    ASSERT_OK(
        mapper_.CreateAndMap(buffer_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr, &vmo_));

    // Fill in the VMO.
    auto frame_view =
        MakeMarvellFrameView(reinterpret_cast<uint8_t*>(mapper_.start()), frame_size_with_header);
    frame_view.header().total_frame_size().Write(frame_size_with_header);
    frame_view.header().channel_id().Write(static_cast<uint8_t>(channel_id_));
    memcpy(frame_view.payload().BackingStorage().data(), data_.data(), data_.size());
    buffer_region_.buffer.vmo = vmo_.get();
    buffer_region_.type = SDMMC_BUFFER_TYPE_VMO_HANDLE;
    buffer_region_.offset = 0;
    buffer_region_.size = buffer_size;
  }

  // Get an SDIO Txn struct that refers to the VMO.
  sdio_rw_txn_t GetSdioTxn(bool is_write, uint32_t addr) const {
    sdio_rw_txn_t txn;
    txn.addr = addr;
    txn.incr = false;
    txn.write = is_write;
    txn.buffers_count = 1;
    txn.buffers_list = &buffer_region_;
    return txn;
  }

  // Simple accessors
  uint32_t size() const { return static_cast<uint32_t>(data_.size()); }
  const uint8_t* data() const { return data_.data(); }
  ControllerChannelId channel_id() const { return channel_id_; }

 private:
  // Our simple frame generator function
  uint8_t ExpectedPayload(uint32_t index) { return (index % 100); }

  ControllerChannelId channel_id_;
  std::vector<uint8_t> data_;
  fzl::VmoMapper mapper_;
  zx::vmo vmo_;
  sdmmc_buffer_region buffer_region_;
  std::optional<DeviceOracle> device_oracle_;
};

}  // namespace bt_hci_marvell

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_TEST_FRAME_H_
