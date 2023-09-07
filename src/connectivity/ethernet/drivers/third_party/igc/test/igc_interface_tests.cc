/*
 * Copyright (c) 2022 The Fuchsia Authors
 *
 * Permission to use, copy, modify, and/or distribute this software for any
 * purpose with or without fee is hereby granted, provided that the above
 * copyright notice and this permission notice appear in all copies.
 *
 * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
 * WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY
 * SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
 * WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION
 * OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
 * CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
 */

#include <fidl/fuchsia.hardware.network/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include <gtest/gtest.h>

#include "src/connectivity/ethernet/drivers/third_party/igc/igc_driver.h"
#include "src/devices/pci/testing/pci_protocol_fake.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

namespace {

namespace ei = ethernet::igc;

constexpr size_t kFakeBarSize = 0x20000;
constexpr uint32_t kTestDeviceId = 0x15F2;
constexpr uint32_t kTestSubsysId = 0x0001;
constexpr uint32_t kTestSubsysVendorId = 0x0abe;
constexpr uint32_t kTestCommand = 0x0033;
constexpr uint8_t kVmoId = 0;

class IgcInterfaceTest : public gtest::RealLoopFixture {
 public:
  IgcInterfaceTest() : outgoing_(dispatcher()) {}

  void SetUp() override {
    RealLoopFixture::SetUp();
    parent_ = MockDevice::FakeRootParent();

    pci::FakePciProtocol fake_pci;
    // Set up the first BAR.
    fake_pci_.CreateBar(/*bar_id=*/0, /*size=*/kFakeBarSize, /*is_mmio=*/true);

    // Identify as the correct device.
    fake_pci_.SetDeviceInfo({.device_id = kTestDeviceId});

    // Setup the configuration for fake PCI.
    zx::unowned_vmo config = fake_pci_.GetConfigVmo();
    config->write(&kTestSubsysId, fidl::ToUnderlying(fuchsia_hardware_pci::Config::kSubsystemId),
                  sizeof(kTestSubsysId));
    config->write(&kTestSubsysVendorId,
                  fidl::ToUnderlying(fuchsia_hardware_pci::Config::kSubsystemVendorId),
                  sizeof(kTestSubsysVendorId));
    config->write(&kTestCommand, fidl::ToUnderlying(fuchsia_hardware_pci::Config::kCommand),
                  sizeof(kTestCommand));

    fake_pci_.AddLegacyInterrupt();

    // Add proper PCI protocol
    auto service_result = outgoing_.AddService<fuchsia_hardware_pci::Service>(
        fuchsia_hardware_pci::Service::InstanceHandler(
            {.device = fake_pci_.bind_handler(dispatcher())}));
    ASSERT_EQ(service_result.status_value(), ZX_OK);

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(endpoints.status_value(), ZX_OK);
    ASSERT_EQ(outgoing_.Serve(std::move(endpoints->server)).status_value(), ZX_OK);

    parent_->AddFidlService(fuchsia_hardware_pci::Service::Name, std::move(endpoints->client),
                            "pci");
    PerformBlockingWork([&] { driver_ = std::make_unique<ei::IgcDriver>(parent_.get()); });

    // Create the VMO like what netdev driver does.
    zx::vmo::create(ei::kEthRxBufCount * ei::kEthRxBufSize + ei::kEthTxBufCount * ei::kEthTxBufSize,
                    kVmoId, &test_vmo_);

    // Initialize the driver, including mapping MmioBuffer.
    PerformBlockingWork([&] { driver_->Init(); });

    // Verify that the netdev has been created during initialization process.
    EXPECT_EQ(1U, parent_->child_count());

    // AddPort() will be called inside this function and will also be verified.
    driver_->NetworkDeviceImplInit(&ifc_);
  }

  void TearDown() override {
    // Release the driver unique_ptr here because mock_ddk will destroy the driver.
    driver_.release();
    device_async_remove(parent_->GetLatestChild());
    PerformBlockingWork([&] { mock_ddk::ReleaseFlaggedDevices(parent_.get()); });
    RealLoopFixture::TearDown();
  }

  // network_device_ifc_protocol_ops_t implementations
  zx_status_t AddPort(uint8_t id, const network_port_protocol_t* port) {
    EXPECT_EQ(id, ei::kPortId);
    return ZX_OK;
  }

  void CompleteRx(const rx_buffer_t* rx_list, size_t rx_count) {
    // Deep copy rx buffers.
    for (size_t i = 0; i < rx_count; i++) {
      rx_buffer_parts_.push_back(*(rx_list[i].data_list));
      rx_buffers_.push_back(rx_list[i]);
    }
  }

  void CompleteTx(const tx_result_t* tx_list, size_t tx_count) {
    // Deep copy tx_list to local record structure since the pointer will point to void after this
    // function returns.
    for (size_t i = 0; i < tx_count; i++) {
      tx_results_.push_back({.id = tx_list[i].id, .status = tx_list[i].status});
    }
  }

  // Fake PCI device that IGC driver talks to in the tests.
  pci::FakePciProtocol fake_pci_;

  // The fake parent zx_device for IGC driver to created child device on.
  std::shared_ptr<MockDevice> parent_;

  // The instance of IGC driver under test.
  std::unique_ptr<ei::IgcDriver> driver_;

  // The directory used to setting connections to fake pci.
  component::OutgoingDirectory outgoing_;

  // The VMO faked by this test class to simulate the one that netdevice creates in real case.
  zx::vmo test_vmo_;

  // The fake protocol handles the calls from driver to netdev driver.
  network_device_ifc_protocol_ops_t ifc_ops_ = {
      .add_port =
          [](void* ctx, uint8_t id, const network_port_protocol_t* port) {
            return ((IgcInterfaceTest*)ctx)->AddPort(id, port);
          },

      .complete_rx =
          [](void* ctx, const rx_buffer_t* rx_list, size_t rx_count) {
            ((IgcInterfaceTest*)ctx)->CompleteRx(rx_list, rx_count);
          },

      .complete_tx =
          [](void* ctx, const tx_result_t* tx_list, size_t tx_count) {
            ((IgcInterfaceTest*)ctx)->CompleteTx(tx_list, tx_count);
          }};

  network_device_ifc_protocol_t ifc_ = {.ops = &ifc_ops_, .ctx = this};

  // The vector to record tx results for each packets from CompleteTx.
  std::vector<tx_result_t> tx_results_;

  // The vector to record rx buffers received from CompleteRx.
  std::vector<rx_buffer_t> rx_buffers_;

  // The local copy of buffer parts received from ComplateRx.
  std::vector<rx_buffer_part_t> rx_buffer_parts_;
};

TEST_F(IgcInterfaceTest, NetworkDeviceImplGetInfo) {
  device_impl_info_t out_info;
  driver_->NetworkDeviceImplGetInfo(&out_info);

  EXPECT_EQ(out_info.tx_depth, ei::kEthTxBufCount);
  EXPECT_EQ(out_info.rx_depth, ei::kEthRxBufCount);
  EXPECT_EQ(out_info.rx_threshold, out_info.rx_depth / 2);
  EXPECT_EQ(out_info.max_buffer_parts, 1U);
  EXPECT_EQ(out_info.max_buffer_length, ZX_PAGE_SIZE / 2);
  EXPECT_EQ(out_info.buffer_alignment, ZX_PAGE_SIZE / 2);
  EXPECT_EQ(out_info.min_rx_buffer_length, 2048U);
  EXPECT_EQ(out_info.min_tx_buffer_length, 60U);
  EXPECT_EQ(out_info.tx_head_length, 0U);
  EXPECT_EQ(out_info.tx_tail_length, 0U);
  EXPECT_EQ(out_info.rx_accel_count, 0U);
  EXPECT_EQ(out_info.tx_accel_count, 0U);
}

TEST_F(IgcInterfaceTest, NetworkDeviceImplPrepareVmo) {
  bool callback_called = false;

  driver_->NetworkDeviceImplPrepareVmo(
      0, std::move(test_vmo_),
      [](void* ctx, zx_status_t s) {
        *((bool*)ctx) = true;
        EXPECT_EQ(s, ZX_OK);
      },
      &callback_called);

  EXPECT_EQ(callback_called, true);
}

TEST_F(IgcInterfaceTest, NetworkDeviceImplStart) {
  bool callback_called = false;

  driver_->NetworkDeviceImplStart(
      [](void* ctx, zx_status_t s) {
        EXPECT_EQ(*((bool*)ctx), false);
        *((bool*)ctx) = true;
        EXPECT_EQ(s, ZX_OK);
      },
      &callback_called);

  EXPECT_EQ(callback_called, true);
}

// If the NetworkDeviceImplStart has not been called, the IGC driver is not in the state which is
// ready for tx, and it will report ZX_ERR_UNAVAILABLE as the status for all the packets that higher
// layer wanted to send. This test case verifies this behavior.
TEST_F(IgcInterfaceTest, NetworkDeviceImplQueueTxNotStarted) {
  constexpr size_t kTxBufferCount = 10;

  // Construct the tx buffer list.
  tx_buffer_t tx_buffer[kTxBufferCount];

  // Populate buffer id only, since the driver won't make use of the data section if not started.
  for (size_t i = 0; i < kTxBufferCount; i++) {
    tx_buffer[i].id = (uint32_t)i;
  }

  driver_->NetworkDeviceImplQueueTx(tx_buffer, kTxBufferCount);

  // Verify the tx results of all packet buffers.
  EXPECT_EQ(tx_results_.size(), kTxBufferCount);
  for (size_t i = 0; i < kTxBufferCount; i++) {
    EXPECT_EQ(tx_results_[i].id, tx_buffer[i].id);
    EXPECT_EQ(tx_results_[i].status, ZX_ERR_UNAVAILABLE);
  }
}

// This test case verifies the normal tx behavior of the IGC driver.
TEST_F(IgcInterfaceTest, NetworkDeviceImplQueueTxStarted) {
  // Pass two buffers in total in this test.
  constexpr size_t kTxBufferCount = 2;
  // Each buffer only contains 1 buffer region.
  constexpr size_t kBufferRegionCount = 1;

  // construct the tx buffer list.
  tx_buffer_t tx_buffer[kTxBufferCount];

  constexpr size_t kFirstRegionLength = 50;
  constexpr size_t kSecondRegionLength = 1024;

  // First buffer.
  buffer_region_t region_list_1[kBufferRegionCount];
  region_list_1[0].vmo = kVmoId;
  region_list_1[0].offset = 0;
  region_list_1[0].length = kFirstRegionLength;

  tx_buffer[0].id = 0;
  tx_buffer[0].data_list = region_list_1;
  tx_buffer[0].data_count = kBufferRegionCount;

  // Second buffer.
  buffer_region_t region_list_2[kBufferRegionCount];
  region_list_2[0].vmo = kVmoId;
  region_list_2[0].offset = 0;
  region_list_2[0].length = kSecondRegionLength;

  tx_buffer[1].id = 1;
  tx_buffer[1].data_list = region_list_2;
  tx_buffer[1].data_count = kBufferRegionCount;

  // Store the VMO into VmoStore.
  driver_->NetworkDeviceImplPrepareVmo(
      kVmoId, std::move(test_vmo_), [](void* ctx, zx_status_t s) {}, nullptr);

  // Call start to change the state of driver.
  driver_->NetworkDeviceImplStart([](void* ctx, zx_status_t s) {}, nullptr);

  driver_->NetworkDeviceImplQueueTx(tx_buffer, kTxBufferCount);

  // Get the access to adapter structure in the IGC driver.
  auto adapter = driver_->Adapter();
  auto driver_tx_buffer_info = driver_->TxBuffer();

  // Verify the data in tx descriptor ring.
  EXPECT_EQ(adapter->txt_ind, kTxBufferCount);
  igc_tx_desc* first_desc_entry = &adapter->txdr[0];
  EXPECT_EQ(first_desc_entry->lower.data,
            (uint32_t)(IGC_TXD_CMD_EOP | IGC_TXD_CMD_IFCS | IGC_TXD_CMD_RS | kFirstRegionLength));
  igc_tx_desc* second_desc_entry = &adapter->txdr[1];
  EXPECT_EQ(second_desc_entry->lower.data,
            (uint32_t)(IGC_TXD_CMD_EOP | IGC_TXD_CMD_IFCS | IGC_TXD_CMD_RS | kSecondRegionLength));

  // Verify the buffer id stored in the internal tx buffer info list.
  EXPECT_EQ(driver_tx_buffer_info[0].buffer_id, 0U);
  EXPECT_EQ(driver_tx_buffer_info[1].buffer_id, 1U);

  // Test the wrap around of ring index.
  tx_buffer_t tx_buffer_extra[ei::kEthTxDescRingCount];

  // All the extra buffers share one buffer region, the content doesn't matter.
  buffer_region_t region_list_extra[kBufferRegionCount];
  region_list_extra[0].vmo = kVmoId;
  region_list_extra[0].offset = 0;
  region_list_extra[0].length = kFirstRegionLength;

  // Send a list of buffer with the count equals to kEthTxDescRingCount, so that the ring buffer
  // index will circle back to the same index.
  for (size_t i = 0; i < ei::kEthTxDescRingCount; i++) {
    tx_buffer_extra[i].id = uint32_t(i + 2);
    tx_buffer_extra[i].data_list = region_list_extra;
    tx_buffer_extra[i].data_count = kBufferRegionCount;
  }

  driver_->NetworkDeviceImplQueueTx(tx_buffer_extra, ei::kEthTxDescRingCount);
  // The tail index of tx descriptor ring buffer stays the same.
  EXPECT_EQ(adapter->txt_ind, kTxBufferCount);
  // But the buffer id at the same slot has been changed.
  EXPECT_EQ(driver_tx_buffer_info[1].buffer_id, ei::kEthTxDescRingCount + 1);
}

TEST_F(IgcInterfaceTest, NetworkDeviceImplQueueRxSpace) {
  // Pass two buffers in total in this test.
  constexpr size_t kRxBufferCount = 2;

  rx_space_buffer_t rx_space_buffer[kRxBufferCount];

  buffer_region_t buffer_region = {
      .vmo = kVmoId,
  };

  // Use the same buffer region for all buffers.
  for (size_t i = 0; i < kRxBufferCount; i++) {
    rx_space_buffer[i].id = (uint32_t)i;
    rx_space_buffer[i].region = buffer_region;
  }

  // Store the VMO into VmoStore.
  driver_->NetworkDeviceImplPrepareVmo(
      kVmoId, std::move(test_vmo_), [](void* ctx, zx_status_t s) {}, nullptr);

  driver_->NetworkDeviceImplQueueRxSpace(rx_space_buffer, kRxBufferCount);

  // Get the access to adapter structure in the IGC driver.
  auto adapter = driver_->Adapter();
  auto driver_rx_buffer_info = driver_->RxBuffer();

  EXPECT_EQ(adapter->rxt_ind, kRxBufferCount);

  EXPECT_EQ(driver_rx_buffer_info[0].buffer_id, 0U);
  EXPECT_TRUE(driver_rx_buffer_info[0].available);
  EXPECT_EQ(driver_rx_buffer_info[1].buffer_id, 1U);
  EXPECT_TRUE(driver_rx_buffer_info[1].available);

  union igc_adv_rx_desc* first_desc_entry = &adapter->rxdr[0];
  EXPECT_EQ(first_desc_entry->wb.upper.status_error, 0U);
  union igc_adv_rx_desc* second_desc_entry = &adapter->rxdr[1];
  EXPECT_EQ(second_desc_entry->wb.upper.status_error, 0U);

  // Test the wrap around of ring index.
  rx_space_buffer_t rx_space_buffer_extra[ei::kEthRxBufCount];

  // Reset the availability states of the first two buffers.
  driver_rx_buffer_info[0].available = false;
  driver_rx_buffer_info[1].available = false;

  // Send extra kEthRxBufCount of buffers.
  for (size_t i = 0; i < ei::kEthRxBufCount; i++) {
    rx_space_buffer_extra[i].id = (uint32_t)i + kRxBufferCount;
    rx_space_buffer_extra[i].region = buffer_region;
  }

  driver_->NetworkDeviceImplQueueRxSpace(rx_space_buffer_extra, ei::kEthRxBufCount);

  // The tail index of rx descriptor ring buffer stays the same.
  EXPECT_EQ(adapter->rxt_ind, kRxBufferCount);
  // But the buffer id at the same slot has been changed.
  EXPECT_EQ(driver_rx_buffer_info[1].buffer_id, ei::kEthRxBufCount + 1);
  EXPECT_TRUE(driver_rx_buffer_info[1].available);
}

// Stop function will return all the rx space buffers with null data, and will also reclaim all the
// tx buffers. This test verifies this behavior.
TEST_F(IgcInterfaceTest, NetworkDeviceImplStop) {
  bool callback_called = false;
  // Get access to adapter structure.
  auto adapter = driver_->Adapter();
  /* Prepare rx space buffers*/
  constexpr size_t kRxBufferCount = 5;

  rx_space_buffer_t rx_space_buffer[kRxBufferCount];

  buffer_region_t buffer_region = {
      .vmo = kVmoId,
  };

  // Use the same buffer region for all buffers.
  for (size_t i = 0; i < kRxBufferCount; i++) {
    rx_space_buffer[i].id = (uint32_t)i;
    rx_space_buffer[i].region = buffer_region;
  }

  /*Prepare tx buffers*/
  constexpr size_t kTxBufferCount = 5;
  constexpr size_t kBufferRegionCount = 1;

  // construct the tx buffer list.
  tx_buffer_t tx_buffer[kTxBufferCount];

  // The buffer regions for each buffer. Assuming each buffer only contains one region in the region
  // list.
  buffer_region_t region_list_1[kTxBufferCount];

  for (size_t i = 0; i < kTxBufferCount; i++) {
    region_list_1[i].vmo = kVmoId;
    region_list_1[i].offset = 0;
    // Set the length of region to 10 times of the index, so that the 5 buffers contain regions with
    // length [0, 10, 20, 30, 40].
    region_list_1[i].length = i * 10;

    tx_buffer[i].id = (uint32_t)i;
    tx_buffer[i].data_list = &region_list_1[i];
    tx_buffer[i].data_count = kBufferRegionCount;
  }

  driver_->NetworkDeviceImplStart([](void* ctx, zx_status_t s) {}, nullptr);

  // Store the VMO into VmoStore.
  driver_->NetworkDeviceImplPrepareVmo(
      kVmoId, std::move(test_vmo_), [](void* ctx, zx_status_t s) {}, nullptr);

  driver_->NetworkDeviceImplQueueRxSpace(rx_space_buffer, kRxBufferCount);

  driver_->NetworkDeviceImplQueueTx(tx_buffer, kTxBufferCount);

  driver_->NetworkDeviceImplStop(
      [](void* ctx) {
        EXPECT_FALSE(*((bool*)ctx));
        *((bool*)ctx) = true;
      },
      &callback_called);

  EXPECT_TRUE(callback_called);

  /*Verify the data comes from CompleteRx and CompleteTx*/

  // CompleteRx data
  EXPECT_EQ(rx_buffers_.size(), kRxBufferCount);
  EXPECT_EQ(rx_buffers_.size(), rx_buffer_parts_.size());
  for (size_t n = 0; n < kRxBufferCount; n++) {
    EXPECT_EQ(rx_buffers_[n].meta.port, ei::kPortId);
    EXPECT_EQ(rx_buffers_[n].meta.frame_type,
              static_cast<uint8_t>(::fuchsia_hardware_network::wire::FrameType::kEthernet));
    EXPECT_EQ(rx_buffers_[n].data_count, 1U);
    EXPECT_EQ(rx_buffer_parts_[n].id, n);
    EXPECT_EQ(rx_buffer_parts_[n].length, 0U);
    EXPECT_EQ(rx_buffer_parts_[n].offset, 0U);
  }

  // The head index should stopped at the tail index of the rx descriptor ring.
  EXPECT_EQ(adapter->rxh_ind, adapter->rxt_ind);

  // CompleteTx data
  EXPECT_EQ(tx_results_.size(), kTxBufferCount);
  for (size_t n = 0; n < kTxBufferCount; n++) {
    EXPECT_EQ(tx_results_[n].id, n);
    EXPECT_EQ(tx_results_[n].status, ZX_ERR_UNAVAILABLE);
  }

  // The head index should stopped at the tail index of the tx descriptor ring.
  EXPECT_EQ(adapter->txh_ind, adapter->txt_ind);
}

// NetworkPort protocol tests
TEST_F(IgcInterfaceTest, NetworkPortGetInfo) {
  port_base_info_t out_info;
  driver_->NetworkPortGetInfo(&out_info);

  EXPECT_EQ(out_info.port_class,
            static_cast<uint32_t>(fuchsia_hardware_network::wire::DeviceClass::kEthernet));
  EXPECT_EQ(out_info.rx_types_list[0],
            static_cast<uint8_t>(fuchsia_hardware_network::wire::FrameType::kEthernet));
  EXPECT_EQ(out_info.rx_types_count, 1U);
  EXPECT_EQ(out_info.tx_types_list[0].type,
            static_cast<uint8_t>(fuchsia_hardware_network::wire::FrameType::kEthernet));
  EXPECT_EQ(out_info.tx_types_list[0].features, fuchsia_hardware_network::wire::kFrameFeaturesRaw);
  EXPECT_EQ(out_info.tx_types_list[0].supported_flags, 0U);
  EXPECT_EQ(out_info.tx_types_count, 1U);
}

TEST_F(IgcInterfaceTest, NetworkPortGetStatus) {
  port_status_t out_status;
  driver_->NetworkPortGetStatus(&out_status);

  EXPECT_EQ(out_status.mtu, ei::kEtherMtu);
  EXPECT_EQ(out_status.flags, 0U);
}

TEST_F(IgcInterfaceTest, NetworkPortGetMac) {
  mac_addr_protocol_t* mac_addr_proto_1 = nullptr;
  mac_addr_protocol_t* mac_addr_proto_2 = nullptr;

  // Verify that the ctx returns from NetworkPortGetMac() is this driver. Additionally, use this
  // driver to call NetworkPortGetMac() again and it returns the same ctx.
  driver_->NetworkPortGetMac(&mac_addr_proto_1);
  ((ei::IgcDriver*)mac_addr_proto_1->ctx)->NetworkPortGetMac(&mac_addr_proto_2);

  EXPECT_EQ(mac_addr_proto_2->ctx, mac_addr_proto_1->ctx);
}

constexpr uint8_t kFakeMacAddr[ei::kEtherAddrLen] = {7, 7, 8, 9, 3, 4};
// MacAddr protocol tests
TEST_F(IgcInterfaceTest, MacAddrGetAddress) {
  // Get the address of driver adapter.
  auto adapter = driver_->Adapter();
  // Set the MAC address to the driver manually.
  memcpy(adapter->hw.mac.addr, kFakeMacAddr, ei::kEtherAddrLen);

  mac_address_t out_mac;

  driver_->MacAddrGetAddress(&out_mac);
  EXPECT_EQ(memcmp(out_mac.octets, kFakeMacAddr, ei::kEtherAddrLen), 0);
}

TEST_F(IgcInterfaceTest, MacAddrGetFeatures) {
  features_t out_feature;
  driver_->MacAddrGetFeatures(&out_feature);

  EXPECT_EQ(out_feature.multicast_filter_count, 0U);
  EXPECT_EQ(out_feature.supported_modes, SUPPORTED_MAC_FILTER_MODE_PROMISCUOUS);
}

}  // namespace
