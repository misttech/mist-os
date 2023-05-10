// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.amlogiccanvas/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.clock/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire_test_base.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/platform-defs.h>
#include <zircon/types.h>

#include <gtest/gtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/media/drivers/amlogic_decoder/device_ctx.h"

namespace amlogic_decoder {
namespace test {

class FakeSysmem : public fidl::testing::WireTestBase<fuchsia_hardware_sysmem::Sysmem> {
 public:
  FakeSysmem() {}

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
};

class FakeCanvas : public fidl::testing::WireTestBase<fuchsia_hardware_amlogiccanvas::Device> {
 public:
  FakeCanvas() {}

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
};

class FakeClock : public fidl::testing::WireTestBase<fuchsia_hardware_clock::Clock> {
 public:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
  FakeSysmem fake_sysmem;
  component::OutgoingDirectory outgoing_sysmem{async_get_default_dispatcher()};
  FakeCanvas fake_canvas;
  component::OutgoingDirectory outgoing_canvas{async_get_default_dispatcher()};
  FakeClock fake_gclk_vdec;
  component::OutgoingDirectory outgoing_gclk_vdec{async_get_default_dispatcher()};
  FakeClock fake_clk_dos;
  component::OutgoingDirectory outgoing_clk_dos{async_get_default_dispatcher()};
};

class BindingTest : public testing::Test {
 protected:
  void InitPdev() {
    fake_pdev::FakePDevFidl::Config config;
    config.use_fake_bti = true;
    config.use_fake_irq = true;

    config.device_info = {
        .pid = PDEV_PID_AMLOGIC_S905D3,
        .mmio_count = 5,
        .irq_count = 4,
    };
    for (uint32_t i = 0; i < config.device_info->mmio_count; i++) {
      // Large enough for any memory range, including AOBUS and CBUS.
      constexpr uint64_t kMmioSize = 0x100000;
      zx::vmo vmo;
      ASSERT_EQ(ZX_OK, zx::vmo::create(kMmioSize, 0, &vmo));
      auto mmio_buffer =
          fdf::MmioBuffer::Create(0, kMmioSize, std::move(vmo), ZX_CACHE_POLICY_CACHED);
      ASSERT_EQ(ZX_OK, mmio_buffer.status_value());

      config.mmios[i] = std::move(*mmio_buffer);
    }

    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(ZX_OK, outgoing_endpoints.status_value());
    ASSERT_EQ(ZX_OK, incoming_loop_.StartThread("incoming-ns-thread"));
    incoming_.SyncCall([config = std::move(config), server = std::move(outgoing_endpoints->server)](
                           IncomingNamespace* infra) mutable {
      infra->pdev_server.SetConfig(std::move(config));
      ASSERT_EQ(ZX_OK, infra->outgoing
                           .AddService<fuchsia_hardware_platform_device::Service>(
                               infra->pdev_server.GetInstanceHandler())
                           .status_value());

      ASSERT_EQ(ZX_OK, infra->outgoing.Serve(std::move(server)).status_value());
    });
    ASSERT_NO_FATAL_FAILURE();
    root_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                          std::move(outgoing_endpoints->client), "pdev");
  }
  void InitSysmem() {
    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(ZX_OK, outgoing_endpoints.status_value());
    incoming_.SyncCall([server = std::move(outgoing_endpoints->server)](
                           IncomingNamespace* infra) mutable {
      ASSERT_EQ(
          ZX_OK,
          infra->outgoing_sysmem
              .AddService<fuchsia_hardware_platform_device::Service>(
                  fuchsia_hardware_sysmem::Service::InstanceHandler(
                      {.sysmem = infra->fake_sysmem.bind_handler(async_get_default_dispatcher())}))
              .status_value());

      ASSERT_EQ(ZX_OK, infra->outgoing_sysmem.Serve(std::move(server)).status_value());
    });
    root_->AddFidlService(fuchsia_hardware_sysmem::Service::Name,
                          std::move(outgoing_endpoints->client), "sysmem-fidl");
  }

  void InitCanvas() {
    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(ZX_OK, outgoing_endpoints.status_value());
    incoming_.SyncCall([server = std::move(outgoing_endpoints->server)](
                           IncomingNamespace* infra) mutable {
      ASSERT_EQ(
          ZX_OK,
          infra->outgoing_canvas
              .AddService<fuchsia_hardware_platform_device::Service>(
                  fuchsia_hardware_amlogiccanvas::Service::InstanceHandler(
                      {.device = infra->fake_canvas.bind_handler(async_get_default_dispatcher())}))
              .status_value());

      ASSERT_EQ(ZX_OK, infra->outgoing_canvas.Serve(std::move(server)).status_value());
    });
    root_->AddFidlService(fuchsia_hardware_amlogiccanvas::Service::Name,
                          std::move(outgoing_endpoints->client), "canvas");
  }

  void InitGclkVdec() {
    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(ZX_OK, outgoing_endpoints.status_value());
    incoming_.SyncCall(
        [server = std::move(outgoing_endpoints->server)](IncomingNamespace* infra) mutable {
          ASSERT_EQ(ZX_OK, infra->outgoing_gclk_vdec
                               .AddService<fuchsia_hardware_platform_device::Service>(
                                   fuchsia_hardware_clock::Service::InstanceHandler(
                                       {.clock = infra->fake_gclk_vdec.bind_handler(
                                            async_get_default_dispatcher())}))
                               .status_value());

          ASSERT_EQ(ZX_OK, infra->outgoing_gclk_vdec.Serve(std::move(server)).status_value());
        });
    root_->AddFidlService(fuchsia_hardware_clock::Service::Name,
                          std::move(outgoing_endpoints->client), "clock-dos-vdec");
  }

  void InitClkDos() {
    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(ZX_OK, outgoing_endpoints.status_value());
    incoming_.SyncCall([server = std::move(outgoing_endpoints->server)](
                           IncomingNamespace* infra) mutable {
      ASSERT_EQ(
          ZX_OK,
          infra->outgoing_clk_dos
              .AddService<fuchsia_hardware_platform_device::Service>(
                  fuchsia_hardware_clock::Service::InstanceHandler(
                      {.clock = infra->fake_clk_dos.bind_handler(async_get_default_dispatcher())}))
              .status_value());

      ASSERT_EQ(ZX_OK, infra->outgoing_clk_dos.Serve(std::move(server)).status_value());
    });
    root_->AddFidlService(fuchsia_hardware_clock::Service::Name,
                          std::move(outgoing_endpoints->client), "clock-dos");
  }

  void InitFirmware() {
    // Firmware that's smaller than the header size will be ignored.
    root_->SetFirmware(std::vector<uint8_t>{0}, "amlogic_video_ucode.bin");
  }

  DeviceCtx* Init() {
    InitPdev();
    InitSysmem();
    InitCanvas();
    InitGclkVdec();
    InitClkDos();
    InitFirmware();
    auto device = std::make_unique<DeviceCtx>(&driver_ctx_, root_.get());
    amlogic_decoder::AmlogicVideo* video = device->video();
    EXPECT_EQ(ZX_OK, video->InitRegisters(root_.get()));
    EXPECT_EQ(ZX_OK, video->InitDecoder());

    EXPECT_EQ(ZX_OK, device->Bind());

    // The root device has taken ownership of the device.
    return device.release();
  }

  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  DriverCtx driver_ctx_;
  std::shared_ptr<MockDevice> root_ = MockDevice::FakeRootParent();
};

TEST_F(BindingTest, Destruction) {
  Init();
  root_.reset();
}

TEST_F(BindingTest, Suspend) {
  auto device = Init();
  ASSERT_EQ(1u, root_->child_count());
  auto* child = root_->GetLatestChild();
  ddk::SuspendTxn txn(device->zxdev(), 0, false, DEVICE_SUSPEND_REASON_REBOOT);
  device->DdkSuspend(std::move(txn));
  child->WaitUntilSuspendReplyCalled();

  root_.reset();
}

}  // namespace test
}  // namespace amlogic_decoder
