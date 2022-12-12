// Copyright (c) 2022 The Fuchsia Authors
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

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/sdio/sdio_bus.h"

#include <fuchsia/hardware/sdio/cpp/banjo-mock.h>
#include <string.h>

#include <wlan/drivers/components/frame.h>
#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/internal_mem_allocator.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/mlan.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/test/mlan_mocks.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

// This is required to use ddk::MockSdio.
bool operator==(const sdio_rw_txn_t& lhs, const sdio_rw_txn_t& rhs) {
  if (lhs.incr == rhs.incr && lhs.write == rhs.write && lhs.addr == rhs.addr &&
      lhs.buffers_count == rhs.buffers_count) {
    for (size_t i = 0; i < lhs.buffers_count; ++i) {
      if (memcmp(&lhs.buffers_list[i], &rhs.buffers_list[i], sizeof(lhs.buffers_list[i])) != 0) {
        return false;
      }
    }
    return true;
  }
  return false;
}

namespace {

using wlan::nxpfmac::SdioBus;

constexpr const char kFunc1ProtoName[] = "sdio-function-1";
constexpr uint32_t kProductSD8987 = 0x9149;
constexpr uint32_t kSdioBlockSize = 256;

struct SdioBusInfo {
  std::unique_ptr<SdioBus> bus;
  mlan_device mlan_dev;
};

template <size_t BufferSize>
struct FrameBufferData {
  FrameBufferData(uint8_t vmo_id, size_t headroom)
      : frame(nullptr, vmo_id, 0, 0, data, static_cast<uint32_t>(sizeof(data)), 0),
        buffer{.pdesc = &frame,
               .pbuf = data,
               .data_offset = static_cast<uint32_t>(headroom),
               .data_len = static_cast<uint32_t>(sizeof(data) - headroom),
               .use_count = 1} {
    frame.ShrinkHead(static_cast<uint32_t>(headroom));
  }
  wlan::drivers::components::Frame frame;
  mlan_buffer buffer;
  uint8_t data[BufferSize];
};

// Create our own MockSdio class that override AckInBandIntr to make it a noop.
class MockSdio : public ddk::MockSdio {
 public:
  void SdioAckInBandIntr() override {
    // Do nothing, this method gets called every IRQ thread loop and can be called multiple times.
    // Adding an expectation for each call becomes tedious and counter-productive.
  }
  zx_status_t SdioRegisterVmo(uint32_t vmo_id, zx::vmo vmo, uint64_t offset, uint64_t size,
                              uint32_t vmo_rights) override {
    return ZX_OK;
  }
  zx_status_t SdioUnregisterVmo(uint32_t vmo_id, zx::vmo* out_vmo) override { return ZX_OK; }
};

class SdioBusTest : public zxtest::Test {
 public:
  void SetUp() override {
    const sdio_protocol_t* sdio_proto = sdio_.GetProto();
    parent_ = MockDevice::FakeRootParent();
    parent_->AddProtocol(ZX_PROTOCOL_SDIO, sdio_proto->ops, sdio_proto->ctx, kFunc1ProtoName);
  }

  void TearDown() override { ASSERT_NO_FATAL_FAILURE(sdio_.VerifyAndClear()); }

  void CreateBus(SdioBusInfo* out_info) {
    // Create an interrupt and a duplicate so that we can hold on to a handle to the interrupt.
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &interrupt_));
    zx::interrupt interrupt;
    ASSERT_OK(interrupt_.duplicate(ZX_RIGHT_SAME_RIGHTS, &interrupt));

    // Add the bare minimum verifications used during construction and destruction of the bus.
    sdio_.ExpectGetDevHwInfo(ZX_OK, hw_info_)
        .ExpectEnableFn(ZX_OK)
        .ExpectGetInBandIntr(ZX_OK, std::move(interrupt))
        .ExpectEnableFnIntr(ZX_OK)
        .ExpectUpdateBlockSize(ZX_OK, MLAN_SDIO_BLOCK_SIZE, false)
        .ExpectDisableFnIntr(ZX_OK)  // Upon destruction the bus will disable these
        .ExpectDisableFn(ZX_OK);

    ASSERT_OK(SdioBus::Create(parent_.get(), &out_info->mlan_dev, &out_info->bus));
    ASSERT_NOT_NULL(out_info->mlan_dev.pmoal_handle);
    ASSERT_NOT_NULL(out_info->bus.get());
    // It's very important that we use the adapter from MlanMockAdapter so that our link-time
    // substituted mlan functions will work correctly.
    out_info->bus->OnMlanRegistered(mlan_adapter_.GetAdapter());
  }

 protected:
  MockSdio sdio_;
  std::shared_ptr<MockDevice> parent_;
  zx::interrupt interrupt_;

  // Indicate card type for this function.
  sdio_hw_info_t hw_info_{.func_hw_info = {.product_id = kProductSD8987}};

  // Mock function called when SdioBus calls mlan_interrupt.
  std::function<mlan_status(t_u16, t_void*)> on_mlan_interrupt_;
  // Mock function called when SdioBus calls mlan_main_process.
  std::function<mlan_status(t_void*)> on_mlan_main_process_;
  wlan::nxpfmac::MlanMockAdapter mlan_adapter_;
};

TEST_F(SdioBusTest, Constructible) {
  SdioBusInfo bus;
  CreateBus(&bus);
}

TEST_F(SdioBusTest, OnFirmwareInitialized) {
  sync_completion_t on_interrupt;
  sync_completion_t on_main_process;

  mlan_adapter_.SetOnMlanInterrupt([&](t_u16 msg_id, t_void* padapter) {
    sync_completion_signal(&on_interrupt);
    return MLAN_STATUS_SUCCESS;
  });
  mlan_adapter_.SetOnMlanMainProcess([&](t_void* padapter) {
    sync_completion_signal(&on_main_process);
    return MLAN_STATUS_SUCCESS;
  });

  SdioBusInfo bus;
  CreateBus(&bus);

  // This should start the IRQ thread and wait for the interrupt.
  bus.bus->OnFirmwareInitialized();

  ASSERT_OK(interrupt_.trigger(0, zx::time()));

  ASSERT_OK(sync_completion_wait(&on_interrupt, ZX_TIME_INFINITE));
  ASSERT_OK(sync_completion_wait(&on_main_process, ZX_TIME_INFINITE));
}

TEST_F(SdioBusTest, TriggerMainProcess) {
  sync_completion_t on_main_process;

  mlan_adapter_.SetOnMlanMainProcess([&](t_void* padapter) {
    sync_completion_signal(&on_main_process);
    return MLAN_STATUS_SUCCESS;
  });

  SdioBusInfo bus;
  CreateBus(&bus);

  // This should trigger the main process.
  ASSERT_OK(bus.bus->TriggerMainProcess());

  // And processing should occur.
  ASSERT_OK(sync_completion_wait(&on_main_process, ZX_TIME_INFINITE));
}

TEST_F(SdioBusTest, ReadRegister) {
  SdioBusInfo bus;
  CreateBus(&bus);

  // Should have been populated when the SdioBus object was created.
  ASSERT_NOT_NULL(bus.mlan_dev.callbacks.moal_read_reg);

  constexpr uint32_t kAddress = 0x1237573;
  constexpr uint8_t kOutValue = 0x42;

  sdio_.ExpectDoRwByte(ZX_OK, false, kAddress, 0, kOutValue);

  uint32_t value = 0;
  ASSERT_EQ(MLAN_STATUS_SUCCESS,
            bus.mlan_dev.callbacks.moal_read_reg(bus.mlan_dev.pmoal_handle, kAddress, &value));
  ASSERT_EQ(kOutValue, value);
}

TEST_F(SdioBusTest, WriteRegister) {
  SdioBusInfo bus;
  CreateBus(&bus);

  // Should have been populated when the SdioBus object was created.
  ASSERT_NOT_NULL(bus.mlan_dev.callbacks.moal_write_reg);

  constexpr uint32_t kAddress = 0x23476554;
  constexpr uint8_t kValue = 0x23;

  sdio_.ExpectDoRwByte(ZX_OK, true, kAddress, kValue, 0);

  ASSERT_EQ(MLAN_STATUS_SUCCESS,
            bus.mlan_dev.callbacks.moal_write_reg(bus.mlan_dev.pmoal_handle, kAddress, kValue));
}

TEST_F(SdioBusTest, ReadDataSyncTxn) {
  SdioBusInfo bus;
  CreateBus(&bus);

  // Should have been populated when the SdioBus object was created.
  ASSERT_NOT_NULL(bus.mlan_dev.callbacks.moal_read_data_sync);

  constexpr uint32_t kPort = 0x87543;
  constexpr uint32_t kVmoId = 9;
  constexpr uint32_t kOffset = 4;
  constexpr uint32_t kDataSize = 3 * kSdioBlockSize;
  constexpr size_t kDataBufferSize = kDataSize + kOffset;

  sdio_.mock_do_rw_txn().ExpectCallWithMatcher([&](sdio_rw_txn_t txn) {
    EXPECT_EQ(kPort & 0xfffff, txn.addr);
    EXPECT_EQ(1u, txn.buffers_count);
    EXPECT_EQ(SDMMC_BUFFER_TYPE_VMO_ID, txn.buffers_list[0].type);
    EXPECT_EQ(kVmoId, txn.buffers_list[0].buffer.vmo_id);
    EXPECT_EQ(kDataSize, txn.buffers_list[0].size);
    EXPECT_FALSE(txn.incr);
    EXPECT_FALSE(txn.write);
    EXPECT_EQ(kOffset, txn.buffers_list[0].offset);
    return std::tuple<zx_status_t>(ZX_OK);
  });

  FrameBufferData<kDataBufferSize> buffer_data(kVmoId, kOffset);

  ASSERT_EQ(MLAN_STATUS_SUCCESS,
            bus.mlan_dev.callbacks.moal_read_data_sync(
                bus.mlan_dev.pmoal_handle, &buffer_data.buffer, kPort, 0 /* timeout */));
}

// Allocate a buffer from the internal memory allocator for a read request and pass it on to SDIO.
TEST_F(SdioBusTest, ReadDataSyncTxnWithRegion) {
  SdioBusInfo bus;
  CreateBus(&bus);

  // Should have been populated when the SdioBus object was created.
  ASSERT_NOT_NULL(bus.mlan_dev.callbacks.moal_read_data_sync);

  constexpr uint32_t kPort = 0x87543;
  constexpr uint32_t kDataSize = 3 * kSdioBlockSize;
  constexpr size_t kDataBufferSize = kDataSize;
  constexpr size_t kDefaultVmoSize = 4096;
  uint32_t vmo_id;
  uint64_t vmo_offset;

  sdio_.mock_do_rw_txn().ExpectCallWithMatcher([&](sdio_rw_txn_t txn) {
    EXPECT_EQ(kPort & 0xfffff, txn.addr);
    EXPECT_EQ(1u, txn.buffers_count);
    EXPECT_EQ(SDMMC_BUFFER_TYPE_VMO_ID, txn.buffers_list[0].type);
    EXPECT_EQ(vmo_id, txn.buffers_list[0].buffer.vmo_id);
    EXPECT_EQ(kDataSize, txn.buffers_list[0].size);
    EXPECT_FALSE(txn.incr);
    EXPECT_FALSE(txn.write);
    EXPECT_EQ(vmo_offset, txn.buffers_list[0].offset);
    return std::tuple<zx_status_t>(ZX_OK);
  });

  // Create the mem allocator and set in the Device Context
  std::unique_ptr<wlan::nxpfmac::InternalMemAllocator> internal_mem_allocator;
  ASSERT_EQ(ZX_OK, wlan::nxpfmac::InternalMemAllocator::Create(bus.bus.get(), kDefaultVmoSize,
                                                               &internal_mem_allocator));
  wlan::nxpfmac::DeviceContext* dev_context =
      reinterpret_cast<wlan::nxpfmac::DeviceContext*>(bus.mlan_dev.pmoal_handle);
  dev_context->internal_mem_allocator_ = internal_mem_allocator.get();

  // Allocate a buffer from the allocator and verify the read request.
  void* buffer = dev_context->internal_mem_allocator_->Alloc(kDataBufferSize);
  ASSERT_NE(nullptr, buffer);
  ASSERT_EQ(true, dev_context->internal_mem_allocator_->GetInternalVmoInfo(
                      reinterpret_cast<uint8_t*>(buffer), &vmo_id, &vmo_offset));
  mlan_buffer mlan_buf = {};
  mlan_buf.pbuf = reinterpret_cast<uint8_t*>(buffer);
  mlan_buf.data_len = kDataBufferSize;

  ASSERT_EQ(MLAN_STATUS_SUCCESS, bus.mlan_dev.callbacks.moal_read_data_sync(
                                     bus.mlan_dev.pmoal_handle, &mlan_buf, kPort, 0 /* timeout */));
  dev_context->internal_mem_allocator_->Free(buffer);
}

TEST_F(SdioBusTest, WriteDataSyncTxn) {
  SdioBusInfo bus;
  CreateBus(&bus);

  // Should have been populated when the SdioBus object was created.
  ASSERT_NOT_NULL(bus.mlan_dev.callbacks.moal_read_data_sync);

  constexpr uint32_t kPort = 0x87543;
  constexpr uint32_t kVmoId = 9;
  constexpr uint32_t kOffset = 4;
  constexpr uint32_t kDataSize = 3 * kSdioBlockSize;
  constexpr size_t kDataBufferSize = kDataSize + kOffset;

  sdio_.mock_do_rw_txn().ExpectCallWithMatcher([&](sdio_rw_txn_t txn) {
    EXPECT_EQ(kPort & 0xfffff, txn.addr);
    EXPECT_EQ(1u, txn.buffers_count);
    EXPECT_EQ(SDMMC_BUFFER_TYPE_VMO_ID, txn.buffers_list[0].type);
    EXPECT_EQ(kVmoId, txn.buffers_list[0].buffer.vmo_id);
    EXPECT_EQ(kDataSize, txn.buffers_list[0].size);
    EXPECT_FALSE(txn.incr);
    EXPECT_TRUE(txn.write);
    EXPECT_EQ(kOffset, txn.buffers_list[0].offset);
    return std::tuple<zx_status_t>(ZX_OK);
  });

  FrameBufferData<kDataBufferSize> buffer_data(kVmoId, kOffset);

  ASSERT_EQ(MLAN_STATUS_SUCCESS,
            bus.mlan_dev.callbacks.moal_write_data_sync(
                bus.mlan_dev.pmoal_handle, &buffer_data.buffer, kPort, 0 /* timeout */));
}

// Allocate a buffer from the internal memory allocator for a write request and pass it on to SDIO.
TEST_F(SdioBusTest, WriteDataSyncTxnWithRegion) {
  SdioBusInfo bus;
  CreateBus(&bus);

  // Should have been populated when the SdioBus object was created.
  ASSERT_NOT_NULL(bus.mlan_dev.callbacks.moal_read_data_sync);

  constexpr uint32_t kPort = 0x87543;
  constexpr uint32_t kDataSize = 3 * kSdioBlockSize;
  constexpr size_t kDataBufferSize = kDataSize;
  constexpr size_t kDefaultVmoSize = 4096;
  uint32_t vmo_id;
  uint64_t vmo_offset;

  sdio_.mock_do_rw_txn().ExpectCallWithMatcher([&](sdio_rw_txn_t txn) {
    EXPECT_EQ(kPort & 0xfffff, txn.addr);
    EXPECT_EQ(1u, txn.buffers_count);
    EXPECT_EQ(SDMMC_BUFFER_TYPE_VMO_ID, txn.buffers_list[0].type);
    EXPECT_EQ(vmo_id, txn.buffers_list[0].buffer.vmo_id);
    EXPECT_EQ(kDataSize, txn.buffers_list[0].size);
    EXPECT_FALSE(txn.incr);
    EXPECT_TRUE(txn.write);
    EXPECT_EQ(vmo_offset, txn.buffers_list[0].offset);
    return std::tuple<zx_status_t>(ZX_OK);
  });

  // Create the mem allocator and set in the Device Context
  std::unique_ptr<wlan::nxpfmac::InternalMemAllocator> internal_mem_allocator;
  ASSERT_EQ(ZX_OK, wlan::nxpfmac::InternalMemAllocator::Create(bus.bus.get(), kDefaultVmoSize,
                                                               &internal_mem_allocator));
  wlan::nxpfmac::DeviceContext* dev_context =
      reinterpret_cast<wlan::nxpfmac::DeviceContext*>(bus.mlan_dev.pmoal_handle);
  dev_context->internal_mem_allocator_ = internal_mem_allocator.get();

  // Allocate a buffer from the allocator and verify the write request.
  void* buffer = dev_context->internal_mem_allocator_->Alloc(kDataBufferSize);
  ASSERT_NE(nullptr, buffer);
  dev_context->internal_mem_allocator_->GetInternalVmoInfo(reinterpret_cast<uint8_t*>(buffer),
                                                           &vmo_id, &vmo_offset);
  mlan_buffer mlan_buf = {};
  mlan_buf.pbuf = reinterpret_cast<uint8_t*>(buffer);
  mlan_buf.data_len = kDataBufferSize;
  ASSERT_EQ(MLAN_STATUS_SUCCESS, bus.mlan_dev.callbacks.moal_write_data_sync(
                                     bus.mlan_dev.pmoal_handle, &mlan_buf, kPort, 0 /* timeout */));
  dev_context->internal_mem_allocator_->Free(buffer);
}

}  // namespace
