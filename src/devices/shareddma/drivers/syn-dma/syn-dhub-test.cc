// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "syn-dhub.h"

#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/clock.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <mock-mmio-reg/mock-mmio-reg.h>
#include <soc/as370/as370-clk.h>
#include <soc/as370/as370-hw.h>
#include <zxtest/zxtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace as370 {

class SynDhubWrapper : public SynDhub {
 public:
  SynDhubWrapper(ddk_mock::MockMmioRegRegion& mmio, uint32_t dma_id)
      : SynDhub(nullptr, ddk::MmioBuffer(mmio.GetMmioBuffer())), dma_id_(dma_id) {}
  void Enable(bool enable) { SynDhub::Enable(dma_id_, enable); }
  void SetBuffer(zx_paddr_t buf, size_t len) { SynDhub::SetBuffer(dma_id_, buf, len); }
  void StartDma() { SynDhub::StartDma(dma_id_, true); }
  void Init() { SynDhub::Init(dma_id_); }
  size_t NumberOfConcurrentDmas() override {
    return 1;
  }  // With this class we test non-concurrent DMAs.

 private:
  uint32_t dma_id_;
};

TEST(SynDhubTest, ConstructForChannel0) {
  auto regs = std::make_unique<ddk_mock::MockMmioReg[]>(as370::kAudioDhubSize / 4);
  ddk_mock::MockMmioRegRegion region(regs.get(), 4, as370::kAudioDhubSize / 4);

  // Stop and clear FIFO for cmd and data.
  regs[0x1'0a04 / 4].ExpectWrite(0x0000'0000);
  regs[0x1'0a08 / 4].ExpectWrite(0x0000'0001);
  regs[0x1'0a14 / 4].ExpectWrite(0x0000'0000);
  regs[0x1'0a18 / 4].ExpectWrite(0x0000'0001);

  // Stop and configure channel.
  regs[0x1'0d18 / 4].ExpectWrite(0x0000'0000);  // Stop.
  regs[0x1'0d00 / 4].ExpectWrite(0x0000'0004);  // MTU = 2 ^ 4 x 8 = 128.

  // FIFO cmd configure and start.
  regs[0x1'0a00 / 4].ExpectWrite(0x0000'0000);  // Base = 0.
  regs[0x1'0600 / 4].ExpectWrite(0x0000'0004);  // Cell depth = 4.
  regs[0x1'0a04 / 4].ExpectWrite(0x0000'0001);  // Start.

  // FIFO data configure and start.
  regs[0x1'0a10 / 4].ExpectWrite(0x0000'0020);  // Base = 32.
  regs[0x1'0618 / 4].ExpectWrite(0x0000'003c);  // Cell depth = 60.
  regs[0x1'0a14 / 4].ExpectWrite(0x0000'0001);  // Start.

  // Channel configure and start.
  regs[0x1'0d18 / 4].ExpectWrite(0x0000'0001);  // Start.
  regs[0x1'0100 / 4].ExpectWrite(0x0000'0001);  // Cell depth = 1.

  // interrupt setup.
  regs[0x1'040c / 4].ExpectRead(0xffff'ffff).ExpectWrite(0xffff'ffff);  // Clear.
  regs[0x1'0104 / 4].ExpectWrite(0x0000'0002);                          // Enable "full" interrupt.

  SynDhubWrapper test(region, DmaId::kDmaIdMa0);
  test.Init();

  region.VerifyAll();
}

TEST(SynDhubTest, EnableChannel0) {
  auto regs = std::make_unique<ddk_mock::MockMmioReg[]>(as370::kAudioDhubSize / 4);
  ddk_mock::MockMmioRegRegion region(regs.get(), 4, as370::kAudioDhubSize / 4);

  SynDhubWrapper test(region, DmaId::kDmaIdMa0);

  regs[0x1'0a04 / 4].ExpectWrite(0x0000'0000);  // Stop FIFO cmd queue.
  regs[0x1'0d18 / 4].ExpectWrite(0x0000'0000);  // Stop channel.
  regs[0x1'0d1c / 4].ExpectWrite(0x0000'0001);  // Clear channel.
  regs[0x1'0f40 / 4].ExpectRead(0x0000'0000);   // Not busy.
  regs[0x1'0f44 / 4].ExpectRead(0x0000'0000);   // Not pending.

  // Stop and clear FIFO for cmd and data.
  regs[0x1'0a04 / 4].ExpectWrite(0x0000'0000);
  regs[0x1'0a08 / 4].ExpectWrite(0x0000'0001);
  regs[0x1'0c00 / 4].ExpectRead(0x0000'0000);  // FIFO not busy.
  regs[0x1'0a14 / 4].ExpectWrite(0x0000'0000);
  regs[0x1'0a18 / 4].ExpectWrite(0x0000'0001);
  regs[0x1'0c00 / 4].ExpectRead(0x0000'0000);  // FIFO not busy.

  regs[0x1'0d18 / 4].ExpectWrite(0x0000'0001);  // Start channel.
  regs[0x1'0a04 / 4].ExpectWrite(0x0000'0001);  // Start cmd queue.
  regs[0x1'0a14 / 4].ExpectWrite(0x0000'0001);  // Start data queue.

  // We do not check for the enable DMA register writes.

  test.Enable(true);

  region.VerifyAll();
}

TEST(SynDhubTest, EnableChannel10) {
  auto regs = std::make_unique<ddk_mock::MockMmioReg[]>(as370::kAudioDhubSize / 4);
  ddk_mock::MockMmioRegRegion region(regs.get(), 4, as370::kAudioDhubSize / 4);

  SynDhubWrapper test(region, DmaId::kDmaIdPdmW0);

  regs[0x1'0b44 / 4].ExpectWrite(0x0000'0000);  // Stop FIFO cmd queue.
  regs[0x1'0e80 / 4].ExpectWrite(0x0000'0000);  // Stop channel.
  regs[0x1'0e84 / 4].ExpectWrite(0x0000'0001);  // Clear channel.
  regs[0x1'0f40 / 4].ExpectRead(0x0000'0000);   // Not busy.
  regs[0x1'0f44 / 4].ExpectRead(0x0000'0000);   // Not pending.

  // Stop and clear FIFO for cmd and data.
  regs[0x1'0b44 / 4].ExpectWrite(0x0000'0000);
  regs[0x1'0b48 / 4].ExpectWrite(0x0000'0001);
  regs[0x1'0c00 / 4].ExpectRead(0x0000'0000);  // FIFO not busy.
  regs[0x1'0b54 / 4].ExpectWrite(0x0000'0000);
  regs[0x1'0b58 / 4].ExpectWrite(0x0000'0001);
  regs[0x1'0c00 / 4].ExpectRead(0x0000'0000);  // FIFO not busy.

  regs[0x1'0e80 / 4].ExpectWrite(0x0000'0001);  // Start channel.
  regs[0x1'0b44 / 4].ExpectWrite(0x0000'0001);  // Start cmd queue.
  regs[0x1'0b54 / 4].ExpectWrite(0x0000'0001);  // Start data queue.

  test.Enable(true);

  region.VerifyAll();
}

TEST(SynDhubTest, DisableChannel0) {
  auto regs = std::make_unique<ddk_mock::MockMmioReg[]>(as370::kAudioDhubSize / 4);
  ddk_mock::MockMmioRegRegion region(regs.get(), 4, as370::kAudioDhubSize / 4);

  SynDhubWrapper test(region, DmaId::kDmaIdMa0);

  regs[0x1'0a04 / 4].ExpectWrite(0x0000'0000);  // Stop FIFO cmd queue.
  regs[0x1'0d18 / 4].ExpectWrite(0x0000'0000);  // Stop channel.
  regs[0x1'0d1c / 4].ExpectWrite(0x0000'0001);  // Clear channel.
  regs[0x1'0f40 / 4].ExpectRead(0x0000'0000);   // Not busy.
  regs[0x1'0f44 / 4].ExpectRead(0x0000'0000);   // Not pending.

  // Stop and clear FIFO for cmd and data.
  regs[0x1'0a04 / 4].ExpectWrite(0x0000'0000);
  regs[0x1'0a08 / 4].ExpectWrite(0x0000'0001);
  regs[0x1'0c00 / 4].ExpectRead(0x0000'0000);  // FIFO not busy.
  regs[0x1'0a14 / 4].ExpectWrite(0x0000'0000);
  regs[0x1'0a18 / 4].ExpectWrite(0x0000'0001);
  regs[0x1'0c00 / 4].ExpectRead(0x0000'0000);  // FIFO not busy.

  test.Enable(false);

  region.VerifyAll();
}

TEST(SynDhubTest, StartDmaForChannel0) {
  auto regs = std::make_unique<ddk_mock::MockMmioReg[]>(as370::kAudioDhubSize / 4);
  ddk_mock::MockMmioRegRegion region(regs.get(), 4, as370::kAudioDhubSize / 4);

  SynDhubWrapper test(region, DmaId::kDmaIdMa0);
  test.Enable(true);
  constexpr uint32_t address = 0x12345678;
  test.SetBuffer(address, 0x8192);

  regs[0x1'0500 / 4].ExpectRead(0x0000'0000);   // Ptr to use.
  regs[0x0'0000 / 4].ExpectWrite(address);      // Address at the ptr location.
  regs[0x0'0004 / 4].ExpectWrite(0x1001'0040);  // Size = 60 (in MTUs).
  regs[0x1'0900 / 4].ExpectWrite(0x0000'0100);  // Push cmd id 0.

  test.StartDma();

  region.VerifyAll();
}

class FakeMmio {
 public:
  explicit FakeMmio(uint32_t size) : reg_count_(size / sizeof(uint32_t)) {  // in 32 bits chunks.
    regs_ = std::make_unique<ddk_fake::FakeMmioReg[]>(reg_count_);
    mmio_ =
        std::make_unique<ddk_fake::FakeMmioRegRegion>(regs_.get(), sizeof(uint32_t), reg_count_);
  }

  fdf::MmioBuffer mmio() { return fdf::MmioBuffer(mmio_->GetMmioBuffer()); }
  ddk_fake::FakeMmioReg& reg(size_t ix) {
    return regs_[ix >> 2];  // Registers are in virtual address units.
  }

 private:
  const size_t reg_count_;
  std::unique_ptr<ddk_fake::FakeMmioReg[]> regs_;
  std::unique_ptr<ddk_fake::FakeMmioRegRegion> mmio_;
};

struct SynDhubLocal : public SynDhub {
  shared_dma_protocol_t GetProto() { return {&this->shared_dma_protocol_ops_, this}; }
  bool AllowNonContiguousRingBuffer() override { return true; }
  SynDhubLocal(zx_device_t* device, ddk::MmioBuffer mmio) : SynDhub(device, std::move(mmio)) {}
  void Shutdown() { SynDhub::Shutdown(); }
  zx_status_t Bind() { return SynDhub::Bind(); }
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

class DhubTest : public zxtest::Test {
 public:
  DhubTest() : mmio_(as370::kAudioDhubSize) {}
  void SetUp() override {
    fake_parent_ = MockDevice::FakeRootParent();

    fake_pdev::FakePDevFidl::Config config;
    config.mmios[0] = mmio_.mmio();
    config.use_fake_bti = true;

    config.irqs[0] = {};
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));
    ASSERT_OK(config.irqs[0].duplicate(ZX_RIGHT_SAME_RIGHTS, &irq_));

    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(outgoing_endpoints);
    ASSERT_OK(incoming_loop_.StartThread("incoming-ns-thread"));
    incoming_.SyncCall([config = std::move(config), server = std::move(outgoing_endpoints->server)](
                           IncomingNamespace* infra) mutable {
      infra->pdev_server.SetConfig(std::move(config));
      ASSERT_OK(infra->outgoing.AddService<fuchsia_hardware_platform_device::Service>(
          infra->pdev_server.GetInstanceHandler()));
      ASSERT_OK(infra->outgoing.Serve(std::move(server)));
    });
    ASSERT_NO_FATAL_FAILURE();
    fake_parent_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                                 std::move(outgoing_endpoints->client));

    ddk::PDevFidl pdev = ddk::PDevFidl(fake_parent_.get());
    ASSERT_TRUE(pdev.is_valid());
    std::optional<ddk::MmioBuffer> mmio;
    ASSERT_OK(pdev.MapMmio(0, &mmio));
    auto server = std::make_unique<SynDhubLocal>(fake_parent_.get(), *std::move(mmio));
    server_ = server.release();  // devmgr is now in charge of the memory for the device.
    ASSERT_OK(server_->Bind());

    auto* child_dev = fake_parent_->GetLatestChild();
    ASSERT_NOT_NULL(child_dev);
    auto dhub = child_dev->GetDeviceContext<SynDhubLocal>();
    auto proto = dhub->GetProto();
    proto_client_ = ddk::SharedDmaProtocolClient(&proto);
  }
  void TearDown() override {
    server_->DdkAsyncRemove();
    mock_ddk::ReleaseFlaggedDevices(fake_parent_.get());
  }

 protected:
  ddk::SharedDmaProtocolClient& proto_client() { return proto_client_; }
  void TriggerInterrupt() { irq_.trigger(0, zx::clock::get_monotonic()); }
  void Shutdown() { server_->Shutdown(); }
  FakeMmio& mmio() { return mmio_; }

 private:
  zx::interrupt irq_;
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  FakeMmio mmio_;
  std::shared_ptr<MockDevice> fake_parent_;
  SynDhubLocal* server_;
  ddk::SharedDmaProtocolClient proto_client_;
};

TEST_F(DhubTest, SizePerNotification) {
  uint32_t size_per_notification = 0;
  dma_notify_callback_t notify = {};
  auto notify_cb = [](void* ctx, dma_state_t state) -> void {};
  notify.callback = notify_cb;
  proto_client().SetNotifyCallback(DmaId::kDmaIdPdmW0, &notify, &size_per_notification);
  ASSERT_EQ(size_per_notification, 16 * 1024);
}

TEST_F(DhubTest, TransferSize) {
  ASSERT_EQ(proto_client().GetTransferSize(DmaId::kDmaIdMa0), 128);
  ASSERT_EQ(proto_client().GetTransferSize(DmaId::kDmaIdPdmW0), 128);
  ASSERT_EQ(proto_client().GetTransferSize(DmaId::kDmaIdPdmW1), 128);
}

TEST_F(DhubTest, OneDmaTwoInterrupts) {
  zx::vmo vmo;
  // Big enough for 8K DmaId::kDmaIdMa0 DMAs.
  constexpr size_t kDmaSize = static_cast<size_t>(32 * 1024);
  proto_client().InitializeAndGetBuffer(DmaId::kDmaIdMa0, DMA_TYPE_CYCLIC, kDmaSize, &vmo);
  mmio().reg(0x1'040c).SetReadCallback([]() {
    return 0x0000'0001;  // interrupt status, always channel 0 triggered.
  });
  proto_client().Start(DmaId::kDmaIdMa0);
  ASSERT_EQ(proto_client().GetBufferPosition(DmaId::kDmaIdMa0), 0);

  TriggerInterrupt();
  TriggerInterrupt();
  Shutdown();  // Such that the thread is done processing.
  // We advanced by 2 DMAs each 8K.
  ASSERT_EQ(proto_client().GetBufferPosition(DmaId::kDmaIdMa0), 16 * 1024);
}

TEST_F(DhubTest, ConcurrentDmas) {
  zx::vmo vmo;
  // Big enough for 8K DmaId::kDmaIdMa0 DMAs.
  constexpr size_t kDmaSize = static_cast<size_t>(32 * 1024);
  proto_client().InitializeAndGetBuffer(DmaId::kDmaIdMa0, DMA_TYPE_CYCLIC, kDmaSize, &vmo);
  mmio().reg(0x1'040c).SetReadCallback([]() {
    return 0x0000'0001;  // interrupt status, always channel 0 triggered.
  });

  std::atomic<int> step = 0;
  mmio().reg(0x1'0502).SetReadCallback([&]() {
    return 0x0000;  // ptr = 0 all the time.
  });
  uint64_t physical_address = 0;
  mmio().reg(0).SetWriteCallback([&](uint64_t val) {  // CommandAddress addr field for base 0 ptr 0.
    if (step == 0) {
      physical_address = val;  // The first DMA set next the physical address.
      step++;
    } else if (step == 1) {
      // The second DMA (concurrent) adds 8K.
      constexpr size_t kDmaOffset = static_cast<size_t>(8 * 1024);
      EXPECT_EQ(val, physical_address + kDmaOffset);
      step++;
    } else {
      EXPECT_TRUE(false, "unexpected write to registor 0");
    }
  });

  proto_client().Start(DmaId::kDmaIdMa0);
  ASSERT_EQ(proto_client().GetBufferPosition(DmaId::kDmaIdMa0), 0);
}

TEST_F(DhubTest, ConcurrentDmasOneInterrupt) {
  zx::vmo vmo;
  // Big enough for 8K DmaId::kDmaIdMa0 DMAs.
  constexpr size_t kDmaSize = static_cast<size_t>(32 * 1024);
  proto_client().InitializeAndGetBuffer(DmaId::kDmaIdMa0, DMA_TYPE_CYCLIC, kDmaSize, &vmo);
  mmio().reg(0x1'040c).SetReadCallback([]() {
    return 0x0000'0001;  // interrupt status, always channel 0 triggered.
  });

  std::atomic<int> step = 0;
  mmio().reg(0x1'0502).SetReadCallback([&]() {
    return 0x0000;  // ptr = 0 all the time.
  });
  uint64_t physical_address = 0;
  mmio().reg(0).SetWriteCallback([&](uint64_t val) {  // CommandAddress addr field for base 0 ptr 0.
    if (step == 0) {
      physical_address = val;  // The first DMA set next the physical address.
      step++;
    } else if (step == 1) {
      // The second DMA (concurrent) adds 8K.
      constexpr size_t kDmaOffset = static_cast<size_t>(8 * 1024);
      EXPECT_EQ(val, physical_address + kDmaOffset);
      step++;
    } else if (step == 2) {
      // The third DMA triggered by the interrupt adds another 8K.
      constexpr size_t kDmaOffset = static_cast<size_t>(2 * 8 * 1024);
      EXPECT_EQ(val, physical_address + kDmaOffset);
      step++;
    } else {
      EXPECT_TRUE(false, "unexpected write to registor 0");
    }
  });

  proto_client().Start(DmaId::kDmaIdMa0);
  ASSERT_EQ(proto_client().GetBufferPosition(DmaId::kDmaIdMa0), 0);
  TriggerInterrupt();
  Shutdown();  // Such that the thread is done processing.
  // One interrupt, we have advanced the position by 8K.
  ASSERT_EQ(proto_client().GetBufferPosition(DmaId::kDmaIdMa0), 8 * 1024);
}

}  // namespace as370
