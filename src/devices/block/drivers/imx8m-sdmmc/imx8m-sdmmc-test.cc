// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "imx8m-sdmmc.h"

#include <fidl/fuchsia.nxp.sdmmc/cpp/wire.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/driver/runtime/testing/cpp/dispatcher.h>
#include <lib/fake-bti/bti.h>
#include <lib/fdf/env.h>
#include <lib/fdf/testing.h>
#include <lib/sdio/hw.h>
#include <lib/sdmmc/hw.h>
#include <lib/zx/clock.h>

#include <atomic>
#include <memory>
#include <optional>
#include <vector>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <zxtest/zxtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

// Stub out vmo_op_range to allow tests to use fake VMOs.
__EXPORT
zx_status_t zx_vmo_op_range(zx_handle_t handle, uint32_t op, uint64_t offset, uint64_t size,
                            void* buffer, size_t buffer_size) {
  return ZX_OK;
}

namespace {

zx_paddr_t PageMask() { return static_cast<uintptr_t>(zx_system_get_page_size()) - 1; }

}  // namespace

namespace imx8m_sdmmc {

class FakeMmio {
 public:
  FakeMmio() : mmio_(sizeof(uint32_t), kRegisterSetSize) {}

  fdf::MmioBuffer mmio() { return fdf::MmioBuffer(mmio_.GetMmioBuffer()); }

  ddk_fake::FakeMmioReg& reg(size_t ix) { return mmio_[ix]; }

 private:
  ddk_fake::FakeMmioRegRegion mmio_;
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

class Imx8mSdmmcTest : public zxtest::Test {
 public:
  void CreateDut(std::vector<zx_paddr_t> dma_paddrs) {
    fake_pdev::FakePDevFidl::Config config;
    config.btis[0] = {};
    dma_paddrs_ = std::move(dma_paddrs);
    ASSERT_OK(fake_bti_create_with_paddrs(dma_paddrs_.data(), dma_paddrs_.size(),
                                          config.btis[0].reset_and_get_address()));
    bti_ = config.btis[0].borrow();
    config.mmios[0] = mmio_.mmio();
    config.device_info = {
        .vid = PDEV_VID_NXP,
        .pid = PDEV_PID_IMX8MMEVK,
        .did = PDEV_DID_IMX_SDHCI,
    };
    config.irqs[0] = {};
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));
    irq_signaller_ = config.irqs[0].borrow();

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
                                 std::move(outgoing_endpoints->client), "pdev");

    const fuchsia_nxp_sdmmc::wire::SdmmcMetadata metadata = {20, 2, 8};
    fit::result encoded = fidl::Persist(metadata);
    ASSERT_TRUE(encoded.is_ok(), "%s", encoded.error_value().FormatDescription().c_str());
    fake_parent_->SetMetadata(DEVICE_METADATA_PRIVATE, encoded.value().data(),
                              encoded.value().size());

    mmio_.reg(PresentState::Get().addr()).SetReadCallback([]() {
      return PresentState::Get()
          .FromValue(0)
          .set_sd_clock_gated_off(1)
          .set_sd_clock_stable(1)
          .reg_value();
    });

    mmio_.reg(HostControllerCapabilities::Get().addr()).SetReadCallback([]() {
      return HostControllerCapabilities::Get()
          .FromValue(0)
          .set_voltage_3v3_support(1)
          .set_voltage_1v8_support(1)
          .set_adma_support(1)
          .set_ddr50_support(1)
          .set_sdr104_support(1)
          .set_sdr50_support(1)
          .set_use_tuning_for_sdr50(1)
          .reg_value();
    });

    zx::result result = fdf::RunOnDispatcherSync(
        DriverDispatcher(), [&]() { EXPECT_OK(Imx8mSdmmc::Create(nullptr, fake_parent_.get())); });
    EXPECT_EQ(ZX_OK, result.status_value());
    ASSERT_EQ(1, fake_parent_->child_count());
    auto* child = fake_parent_->GetLatestChild();
    dut_ = child->GetDeviceContext<Imx8mSdmmc>();
  }

  void CreateDut() { CreateDut({}); }

  void Destroy() {
    auto* child = fake_parent_->GetLatestChild();
    zx::result result = fdf::RunOnDispatcherSync(DriverDispatcher(), [&]() { child->UnbindOp(); });
    EXPECT_EQ(ZX_OK, result.status_value());
    EXPECT_TRUE(child->UnbindReplyCalled());
    result = fdf::RunOnDispatcherSync(DriverDispatcher(), [&]() { child->ReleaseOp(); });
    EXPECT_EQ(ZX_OK, result.status_value());
  }

  void InjectInterrupt() { irq_signaller_->trigger(0, zx::clock::get_monotonic()); }
  async_dispatcher_t* DriverDispatcher() { return test_driver_dispatcher_->async_dispatcher(); }

 protected:
  // TODO(fxb/124464): Migrate test to use dispatcher integration.
  std::shared_ptr<MockDevice> fake_parent_{
      MockDevice::FakeRootParentNoDispatcherIntegrationDEPRECATED()};
  FakeMmio mmio_;
  Imx8mSdmmc* dut_;

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher test_driver_dispatcher_ = runtime_.StartBackgroundDispatcher();
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  std::vector<zx_paddr_t> dma_paddrs_;
  zx::unowned_interrupt irq_signaller_;
  zx::unowned_bti bti_;
};

TEST_F(Imx8mSdmmcTest, DdkLifecycle) {
  ASSERT_NO_FATAL_FAILURE(CreateDut());
  ASSERT_NO_FATAL_FAILURE(Destroy());
}

TEST_F(Imx8mSdmmcTest, HostInfo) {
  sdmmc_host_info_t host_info = {};
  ASSERT_NO_FATAL_FAILURE(CreateDut());
  EXPECT_OK(dut_->SdmmcHostInfo(&host_info));

  EXPECT_EQ(host_info.caps, SDMMC_HOST_CAP_BUS_WIDTH_8 | SDMMC_HOST_CAP_VOLTAGE_330 |
                                SDMMC_HOST_CAP_AUTO_CMD12 | SDMMC_HOST_CAP_SDR50 |
                                SDMMC_HOST_CAP_SDR104 | SDMMC_HOST_CAP_DDR50 | SDMMC_HOST_CAP_DMA);
  EXPECT_EQ(host_info.prefs, 0);

  ASSERT_NO_FATAL_FAILURE(Destroy());
}

TEST_F(Imx8mSdmmcTest, SetSignalVoltage) {
  ASSERT_NO_FATAL_FAILURE(CreateDut());

  mmio_.reg(VendorSpecificRegister::Get().addr()).SetWriteCallback([](uint64_t value) {
    EXPECT_FALSE(VendorSpecificRegister::Get().FromValue(0).set_voltage_select(1).reg_value() &
                 value);
  });
  EXPECT_OK(dut_->SdmmcSetSignalVoltage(SDMMC_VOLTAGE_V330));

  mmio_.reg(VendorSpecificRegister::Get().addr()).SetWriteCallback([](uint64_t value) {
    EXPECT_TRUE(VendorSpecificRegister::Get().FromValue(0).set_voltage_select(1).reg_value() &
                value);
  });
  mmio_.reg(VendorSpecificRegister::Get().addr()).SetReadCallback([]() {
    return VendorSpecificRegister::Get().FromValue(0).set_voltage_select(1).reg_value();
  });
  EXPECT_OK(dut_->SdmmcSetSignalVoltage(SDMMC_VOLTAGE_V180));

  ASSERT_NO_FATAL_FAILURE(Destroy());
}

TEST_F(Imx8mSdmmcTest, RequestCommandOnly) {
  ASSERT_NO_FATAL_FAILURE(CreateDut());

  sdmmc_req_t request = {
      .cmd_idx = SDMMC_SEND_STATUS,
      .cmd_flags = SDMMC_SEND_STATUS_FLAGS,
      .arg = 0x7b7d9fbd,
      .buffers_count = 0,
  };

  mmio_.reg(CommandArgument::Get().addr()).SetWriteCallback([](uint64_t value) {
    EXPECT_EQ(value, 0x7b7d9fbd);
  });

  mmio_.reg(CommandTransferType::Get().addr()).SetWriteCallback([this](uint64_t value) {
    EXPECT_EQ(CommandTransferType::Get()
                  .FromValue(0)
                  .set_cmd_index(SDMMC_SEND_STATUS)
                  .set_cmd_type(CommandTransferType::kCommandTypeNormal)
                  .set_response_type(CommandTransferType::kResponseType48Bits)
                  .set_cmd_crc_check(1)
                  .set_cmd_index_check(1)
                  .reg_value(),
              value);
    InjectInterrupt();
  });

  mmio_.reg(MixerControl::Get().addr()).SetWriteCallback([](uint64_t value) {
    auto mask = MixerControl::Get()
                    .FromValue(0)
                    .set_auto_cmd12_enable(1)
                    .set_auto_cmd23_enable(1)
                    .set_dma_enable(1)
                    .set_block_count_enable(1)
                    .set_data_transfer_dir_select(1)
                    .set_multi_single_block_select(1)
                    .reg_value();
    EXPECT_FALSE(mask & value);
  });

  mmio_.reg(InterruptStatus::Get().addr()).SetReadCallback([]() {
    return InterruptStatus::Get().FromValue(0).set_cmd_complete(1).reg_value();
  });

  uint32_t response[4] = {};
  EXPECT_OK(dut_->SdmmcRequest(&request, response));

  request = {
      .cmd_idx = SDMMC_SEND_CSD,
      .cmd_flags = SDMMC_SEND_CSD_FLAGS,
      .arg = 0x9c1dc1ed,
      .buffers_count = 0,
  };

  mmio_.reg(CommandArgument::Get().addr()).SetWriteCallback([](uint64_t value) {
    EXPECT_EQ(value, 0x9c1dc1ed);
  });

  mmio_.reg(CommandTransferType::Get().addr()).SetWriteCallback([this](uint64_t value) {
    EXPECT_EQ(CommandTransferType::Get()
                  .FromValue(0)
                  .set_cmd_index(SDMMC_SEND_CSD)
                  .set_cmd_type(CommandTransferType::kCommandTypeNormal)
                  .set_response_type(CommandTransferType::kResponseType136Bits)
                  .set_cmd_crc_check(1)
                  .reg_value(),
              value);

    InjectInterrupt();
  });

  EXPECT_OK(dut_->SdmmcRequest(&request, response));
  ASSERT_NO_FATAL_FAILURE(Destroy());
}

TEST_F(Imx8mSdmmcTest, DmaRequest32Bit) {
  ASSERT_NO_FATAL_FAILURE(CreateDut());

  for (int i = 0; i < 4; i++) {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(512 * 16, 0, &vmo));
    EXPECT_OK(
        dut_->SdmmcRegisterVmo(i, 3, std::move(vmo), 64 * i, 512 * 12, SDMMC_VMO_RIGHT_WRITE));
  }

  const sdmmc_buffer_region_t buffers[4] = {
      {
          .buffer =
              {
                  .vmo_id = 1,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 16,
          .size = 512,
      },
      {
          .buffer =
              {
                  .vmo_id = 0,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 32,
          .size = 512 * 3,
      },
      {
          .buffer =
              {
                  .vmo_id = 3,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 48,
          .size = 512 * 10,
      },
      {
          .buffer =
              {
                  .vmo_id = 2,
              },
          .type = SDMMC_BUFFER_TYPE_VMO_ID,
          .offset = 80,
          .size = 512 * 7,
      },
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 512,
      .suppress_error_messages = false,
      .client_id = 3,
      .buffers_list = buffers,
      .buffers_count = std::size(buffers),
  };

  mmio_.reg(InterruptStatus::Get().addr()).SetReadCallback([]() {
    return InterruptStatus::Get()
        .FromValue(0)
        .set_cmd_complete(1)
        .set_transfer_complete(1)
        .reg_value();
  });

  mmio_.reg(CommandTransferType::Get().addr()).SetWriteCallback([this](uint64_t value) {
    InjectInterrupt();
  });

  mmio_.reg(AdmaSystemAddress::Get().addr()).SetWriteCallback([](uint64_t value) {
    EXPECT_EQ(value, zx_system_get_page_size());
  });

  uint32_t response[4] = {};
  EXPECT_OK(dut_->SdmmcRequest(&request, response));

  const auto* const descriptors =
      reinterpret_cast<Imx8mSdmmc::AdmaDescriptor64*>(dut_->iobuf_virt());

  EXPECT_EQ(descriptors[0].attr, 0b100'001);
  EXPECT_EQ(descriptors[0].address, zx_system_get_page_size() + 80);
  EXPECT_EQ(descriptors[0].length, 512);

  EXPECT_EQ(descriptors[1].attr, 0b100'001);
  EXPECT_EQ(descriptors[1].address, zx_system_get_page_size() + 32);
  EXPECT_EQ(descriptors[1].length, 512 * 3);

  // Buffer is greater than one page and gets split across two descriptors.
  EXPECT_EQ(descriptors[2].attr, 0b100'001);
  EXPECT_EQ(descriptors[2].address, zx_system_get_page_size() + 240);
  EXPECT_EQ(descriptors[2].length, zx_system_get_page_size() - 240);

  EXPECT_EQ(descriptors[3].attr, 0b100'001);
  EXPECT_EQ(descriptors[3].address, zx_system_get_page_size());
  EXPECT_EQ(descriptors[3].length, (512 * 10) - zx_system_get_page_size() + 240);

  EXPECT_EQ(descriptors[4].attr, 0b100'011);
  EXPECT_EQ(descriptors[4].address, zx_system_get_page_size() + 208);
  EXPECT_EQ(descriptors[4].length, 512 * 7);

  ASSERT_NO_FATAL_FAILURE(Destroy());
}

TEST_F(Imx8mSdmmcTest, DmaNoBoundaries) {
  constexpr zx_paddr_t kDescriptorAddress = 0xc000'0000;
  const zx_paddr_t kStartAddress = 0xa7ff'ffff & ~PageMask();

  ASSERT_NO_FATAL_FAILURE(CreateDut({
      kDescriptorAddress,
      kStartAddress,
      kStartAddress + zx_system_get_page_size(),
      kStartAddress + (zx_system_get_page_size() * 2),
      0xb000'0000,
  }));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size() * 4, 0, &vmo));
  ASSERT_OK(dut_->SdmmcRegisterVmo(0, 0, std::move(vmo), 0, zx_system_get_page_size() * 4,
                                   SDMMC_VMO_RIGHT_WRITE));

  const sdmmc_buffer_region_t buffer = {
      .buffer =
          {
              .vmo_id = 0,
          },
      .type = SDMMC_BUFFER_TYPE_VMO_ID,
      .offset = zx_system_get_page_size() - 4,
      .size = (zx_system_get_page_size() * 2) + 256,
  };

  const sdmmc_req_t request = {
      .cmd_idx = SDMMC_READ_MULTIPLE_BLOCK,
      .cmd_flags = SDMMC_READ_MULTIPLE_BLOCK_FLAGS,
      .arg = 0x1234abcd,
      .blocksize = 16,
      .suppress_error_messages = false,
      .client_id = 0,
      .buffers_list = &buffer,
      .buffers_count = 1,
  };

  mmio_.reg(InterruptStatus::Get().addr()).SetReadCallback([]() {
    return InterruptStatus::Get()
        .FromValue(0)
        .set_cmd_complete(1)
        .set_transfer_complete(1)
        .reg_value();
  });

  mmio_.reg(CommandTransferType::Get().addr()).SetWriteCallback([this](uint64_t value) {
    InjectInterrupt();
  });

  mmio_.reg(AdmaSystemAddress::Get().addr()).SetWriteCallback([kDescriptorAddress](uint64_t value) {
    EXPECT_EQ(value, kDescriptorAddress);
  });

  uint32_t response[4] = {};
  EXPECT_OK(dut_->SdmmcRequest(&request, response));

  const Imx8mSdmmc::AdmaDescriptor64* const descriptors =
      reinterpret_cast<Imx8mSdmmc::AdmaDescriptor64*>(dut_->iobuf_virt());

  EXPECT_EQ(descriptors[0].attr, 0b100'001);
  EXPECT_EQ(descriptors[0].address, 0xa7ff'fffc);
  EXPECT_EQ(descriptors[0].length, (zx_system_get_page_size() * 2) + 4);

  EXPECT_EQ(descriptors[1].attr, 0b100'011);
  EXPECT_EQ(descriptors[1].address, 0xb000'0000);
  EXPECT_EQ(descriptors[1].length, 256 - 4);

  ASSERT_NO_FATAL_FAILURE(Destroy());
}

}  // namespace imx8m_sdmmc
