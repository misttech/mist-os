// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.device/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/fake-bti/bti.h>
#include <lib/fake-resource/resource.h>
#include <lib/mmio/mmio-buffer.h>
#include <zircon/errors.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <zxtest/zxtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/lib/mmio/test-helper.h"

constexpr uint32_t kVid = 1;
constexpr uint32_t kPid = 1;

namespace fhpd = fuchsia_hardware_platform_device;

TEST(PDevTest, GetInterrupt) {
  constexpr zx_handle_t kFakeHandle = 3;
  pdev_protocol_ops_t fake_ops{
      .get_interrupt =
          [](void* ctx, uint32_t index, uint32_t flags, zx_handle_t* out_irq) {
            *out_irq = kFakeHandle;
            return ZX_OK;
          },
  };

  pdev_protocol_t fake_proto{
      .ops = &fake_ops,
      .ctx = nullptr,
  };

  ddk::PDev pdev(&fake_proto);
  zx::interrupt out;
  ASSERT_OK(pdev.GetInterrupt(0, 0, &out));
  ASSERT_EQ(out.get(), kFakeHandle);
}

class DeviceServer : public fidl::testing::WireTestBase<fuchsia_hardware_platform_device::Device> {
 public:
  zx_status_t Connect(fidl::ServerEnd<fhpd::Device> request) {
    if (binding_.has_value()) {
      return ZX_ERR_ALREADY_BOUND;
    }
    binding_.emplace(async_get_default_dispatcher(), std::move(request), this,
                     fidl::kIgnoreBindingClosure);
    return ZX_OK;
  }

 private:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override {
    fidl::Arena arena;
    completer.ReplySuccess(fuchsia_hardware_platform_device::wire::BoardInfo::Builder(arena)
                               .vid(kVid)
                               .pid(kPid)
                               .Build());
  }
  std::optional<fidl::ServerBinding<fuchsia_hardware_platform_device::Device>> binding_;
};

class FakePDevFidlWithThread {
 public:
  zx::result<fidl::ClientEnd<fuchsia_hardware_platform_device::Device>> Start(
      fake_pdev::FakePDevFidl::Config config) {
    loop_.StartThread("pdev-fidl-thread");
    if (zx_status_t status =
            server.SyncCall(&fake_pdev::FakePDevFidl::SetConfig, std::move(config));
        status != ZX_OK) {
      return zx::error(status);
    }
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_platform_device::Device>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }
    if (zx_status_t status =
            server.SyncCall(&fake_pdev::FakePDevFidl::Connect, std::move(endpoints->server));
        status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(std::move(endpoints->client));
  }

 private:
  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<fake_pdev::FakePDevFidl> server{loop_.dispatcher(),
                                                                      std::in_place};
};

TEST(PDevFidlTest, GetMmios) {
  constexpr uint32_t kMmioId = 5;
  constexpr zx_off_t kMmioOffset = 10;
  constexpr size_t kMmioSize = 11;
  std::map<uint32_t, fake_pdev::Mmio> mmios;
  {
    fake_pdev::MmioInfo mmio{
        .offset = kMmioOffset,
        .size = kMmioSize,
    };
    ASSERT_OK(zx::vmo::create(0, 0, &mmio.vmo));
    mmios[kMmioId] = std::move(mmio);
  }

  FakePDevFidlWithThread infra;
  zx::result client_channel = infra.Start({
      .mmios = std::move(mmios),
  });
  ASSERT_OK(client_channel);

  ddk::PDevFidl pdev{std::move(client_channel.value())};
  pdev_mmio_t mmio;
  ASSERT_OK(pdev.GetMmio(kMmioId, &mmio));
  ASSERT_EQ(kMmioOffset, mmio.offset);
  ASSERT_EQ(kMmioSize, mmio.size);

  ASSERT_EQ(ZX_ERR_NOT_FOUND, pdev.GetMmio(4, &mmio));
}

TEST(PDevFidlTest, GetMmioBuffer) {
  constexpr uint32_t kMmioId = 5;
  constexpr zx_off_t kMmioOffset = 10;
  constexpr size_t kMmioSize = 11;
  MMIO_PTR void* mmio_vaddr;
  zx_koid_t vmo_koid;
  zx_info_handle_basic info;
  std::map<uint32_t, fake_pdev::Mmio> mmios;
  {
    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(kMmioSize, 0, &vmo));
    ASSERT_OK(vmo.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
    zx::result<fdf::MmioBuffer> mmio =
        fdf::MmioBuffer::Create(kMmioOffset, kMmioSize, std::move(vmo), ZX_CACHE_POLICY_UNCACHED);
    ASSERT_OK(mmio.status_value());
    vmo_koid = info.koid;
    mmio_vaddr = mmio->get();
    mmios[kMmioId] = std::move(*mmio);
  }

  FakePDevFidlWithThread infra;
  zx::result client_channel = infra.Start({
      .mmios = std::move(mmios),
  });
  ASSERT_OK(client_channel);

  ddk::PDevFidl pdev{std::move(client_channel.value())};
  pdev_mmio_t mmio = {};
  ASSERT_OK(pdev.GetMmio(kMmioId, &mmio));
  ASSERT_NE(kMmioOffset, mmio.offset);
  ASSERT_EQ(0, mmio.size);
  ASSERT_EQ(ZX_HANDLE_INVALID, mmio.vmo);

  auto* mmio_buffer = reinterpret_cast<fdf::MmioBuffer*>(mmio.offset);
  ASSERT_EQ(kMmioOffset, mmio_buffer->get_offset());
  ASSERT_EQ(kMmioSize, mmio_buffer->get_size());
  ASSERT_EQ(mmio_vaddr, mmio_buffer->get());
  ASSERT_OK(mmio_buffer->get_vmo()->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr,
                                             nullptr));
  ASSERT_EQ(vmo_koid, info.koid);

  ASSERT_EQ(ZX_ERR_NOT_FOUND, pdev.GetMmio(4, &mmio));
}

TEST(PDevFidlTest, InvalidMmioHandle) {
  constexpr uint32_t kMmioId = 5;
  constexpr zx_off_t kMmioOffset = 10;
  constexpr size_t kMmioSize = 11;
  std::map<uint32_t, fake_pdev::Mmio> mmios;
  mmios[kMmioId] = fake_pdev::MmioInfo{
      .offset = kMmioOffset,
      .size = kMmioSize,
  };

  FakePDevFidlWithThread infra;
  zx::result client_channel = infra.Start({
      .mmios = std::move(mmios),
  });
  ASSERT_OK(client_channel);

  ddk::PDevFidl pdev{std::move(client_channel.value())};
  pdev_mmio_t mmio = {};
  ASSERT_NOT_OK(pdev.GetMmio(kMmioId, &mmio));
}

TEST(PDevFidlTest, GetIrqs) {
  constexpr uint32_t kIrqId = 5;
  std::map<uint32_t, zx::interrupt> irqs;
  {
    zx::interrupt irq;
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq));
    irqs[kIrqId] = std::move(irq);
  }

  FakePDevFidlWithThread infra;
  zx::result client_channel = infra.Start({
      .irqs = std::move(irqs),
  });
  ASSERT_OK(client_channel);

  ddk::PDevFidl pdev{std::move(client_channel.value())};
  zx::interrupt irq;
  ASSERT_OK(pdev.GetInterrupt(kIrqId, 0, &irq));
  ASSERT_EQ(ZX_ERR_NOT_FOUND, pdev.GetInterrupt(4, 0, &irq));
}

TEST(PDevFidlTest, GetBtis) {
  constexpr uint32_t kBtiId = 5;
  std::map<uint32_t, zx::bti> btis;
  {
    zx::bti bti;
    ASSERT_OK(fake_bti_create(bti.reset_and_get_address()));
    btis[kBtiId] = std::move(bti);
  }

  FakePDevFidlWithThread infra;
  zx::result client_channel = infra.Start({
      .btis = std::move(btis),
  });
  ASSERT_OK(client_channel);

  ddk::PDevFidl pdev{std::move(client_channel.value())};
  zx::bti bti;
  ASSERT_OK(pdev.GetBti(kBtiId, &bti));
  ASSERT_EQ(ZX_ERR_NOT_FOUND, pdev.GetBti(4, &bti));
}

TEST(PDevFidlTest, GetSmc) {
  constexpr uint32_t kSmcId = 5;
  std::map<uint32_t, zx::resource> smcs;
  {
    zx::resource smc;
    ASSERT_OK(fake_root_resource_create(smc.reset_and_get_address()));
    smcs[kSmcId] = std::move(smc);
  }

  FakePDevFidlWithThread infra;
  zx::result client_channel = infra.Start({
      .smcs = std::move(smcs),
  });
  ASSERT_OK(client_channel);

  ddk::PDevFidl pdev{std::move(client_channel.value())};
  zx::resource smc;
  ASSERT_OK(pdev.GetSmc(kSmcId, &smc));
  ASSERT_EQ(ZX_ERR_NOT_FOUND, pdev.GetSmc(4, &smc));
}

TEST(PDevFidlTest, GetDeviceInfo) {
  FakePDevFidlWithThread infra;
  zx::result client_channel = infra.Start({
      .device_info =
          pdev_device_info_t{
              .vid = kVid,
              .pid = kPid,
          },
  });
  ASSERT_OK(client_channel);

  ddk::PDevFidl pdev{std::move(client_channel.value())};
  pdev_device_info_t device_info;
  ASSERT_OK(pdev.GetDeviceInfo(&device_info));
  ASSERT_EQ(kPid, device_info.pid);
  ASSERT_EQ(kVid, device_info.vid);
}

TEST(PDevFidlTest, GetBoardInfo) {
  FakePDevFidlWithThread infra;
  zx::result client_channel = infra.Start({
      .board_info =
          pdev_board_info_t{
              .vid = kVid,
              .pid = kPid,
          },
  });
  ASSERT_OK(client_channel);

  ddk::PDevFidl pdev{std::move(client_channel.value())};
  pdev_board_info_t board_info;
  ASSERT_OK(pdev.GetBoardInfo(&board_info));
  ASSERT_EQ(kPid, board_info.pid);
  ASSERT_EQ(kVid, board_info.vid);
}
