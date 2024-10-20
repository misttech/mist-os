// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/drivers/misc/goldfish/pipe_device.h"

#include <fidl/fuchsia.hardware.goldfish.pipe/cpp/wire.h>
#include <fidl/fuchsia.hardware.goldfish/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fake-bti/bti.h>
#include <lib/zx/channel.h>
#include <lib/zx/vmar.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>

#include <algorithm>
#include <cstring>
#include <memory>
#include <set>
#include <thread>
#include <vector>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/goldfish/platform/cpp/bind.h>
#include <bind/fuchsia/google/platform/cpp/bind.h>
#include <zxtest/zxtest.h>

#include "src/devices/lib/acpi/mock/mock-acpi.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace goldfish {

using MockAcpiFidl = acpi::mock::Device;

namespace {

constexpr uint32_t kPipeMinDeviceVersion = 2;
constexpr uint32_t kMaxSignalledPipes = 64;

const zx_device_str_prop_t kDefaultPipeDeviceProps[] = {
    ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_VID,
                         bind_fuchsia_google_platform::BIND_PLATFORM_DEV_VID_GOOGLE),
    ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_PID,
                         bind_fuchsia_goldfish_platform::BIND_PLATFORM_DEV_PID_GOLDFISH),
    ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_DID,
                         bind_fuchsia_goldfish_platform::BIND_PLATFORM_DEV_DID_PIPE_CONTROL),
};
constexpr const char* kDefaultPipeDeviceName = "goldfish-pipe";

// MMIO Registers of goldfish pipe.
// The layout should match the register offsets defined in pipe_device.cc.
struct Registers {
  uint32_t command;
  uint32_t signal_buffer_high;
  uint32_t signal_buffer_low;
  uint32_t signal_buffer_count;
  uint32_t reserved0[1];
  uint32_t open_buffer_high;
  uint32_t open_buffer_low;
  uint32_t reserved1[2];
  uint32_t version;
  uint32_t reserved2[3];
  uint32_t get_signalled;

  void DebugPrint() const {
    printf(
        "Registers [ command %08x signal_buffer: %08x %08x count %08x open_buffer: %08x %08x "
        "version %08x get_signalled %08x ]\n",
        command, signal_buffer_high, signal_buffer_low, signal_buffer_count, open_buffer_high,
        open_buffer_low, version, get_signalled);
  }
};

// A RAII memory mapping wrapper of VMO to memory.
class VmoMapping {
 public:
  VmoMapping(const zx::vmo& vmo, size_t size, size_t offset = 0,
             zx_vm_option_t perm = ZX_VM_PERM_READ | ZX_VM_PERM_WRITE)
      : vmo_(vmo), size_(size), offset_(offset), perm_(perm) {
    map();
  }

  ~VmoMapping() { unmap(); }

  void map() {
    if (!ptr_) {
      zx::vmar::root_self()->map(perm_, 0, vmo_, offset_, size_,
                                 reinterpret_cast<uintptr_t*>(&ptr_));
    }
  }

  void unmap() {
    if (ptr_) {
      zx::vmar::root_self()->unmap(reinterpret_cast<uintptr_t>(ptr_), size_);
      ptr_ = nullptr;
    }
  }

  void* ptr() const { return ptr_; }

 private:
  const zx::vmo& vmo_;
  size_t size_ = 0u;
  size_t offset_ = 0u;
  zx_vm_option_t perm_ = 0;
  void* ptr_ = nullptr;
};

// Test suite creating fake PipeDevice on a mock ACPI bus.
class PipeDeviceTest : public zxtest::Test {
 public:
  PipeDeviceTest()
      // The IncomingNamespace must live on a different thread because the
      // pipe-device makes synchronous FIDL calls to it.
      : ns_loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        fake_root_(MockDevice::FakeRootParent()),
        test_loop_(&kAsyncLoopConfigAttachToCurrentThread) {}

  // |zxtest::Test|
  void SetUp() override {
    ASSERT_OK(ns_loop_.StartThread("incoming-namespace-loop-dispatcher"));

    ASSERT_OK(fake_bti_create(acpi_bti_.reset_and_get_address()));

    constexpr size_t kCtrlSize = 4096u;
    ASSERT_OK(zx::vmo::create(kCtrlSize, 0u, &vmo_control_));

    zx::interrupt irq;
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0u, ZX_INTERRUPT_VIRTUAL, &irq));
    ASSERT_OK(irq.duplicate(ZX_RIGHT_SAME_RIGHTS, &irq_));

    mock_acpi_fidl_.SetMapInterrupt(
        [this](acpi::mock::Device::MapInterruptRequestView rv,
               acpi::mock::Device::MapInterruptCompleter::Sync& completer) {
          zx::interrupt dupe;
          ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &dupe));
          ASSERT_OK(irq_.duplicate(ZX_RIGHT_SAME_RIGHTS, &dupe));
          completer.ReplySuccess(std::move(dupe));
        });
    mock_acpi_fidl_.SetGetMmio([this](acpi::mock::Device::GetMmioRequestView rv,
                                      acpi::mock::Device::GetMmioCompleter::Sync& completer) {
      ASSERT_EQ(rv->index, 0);
      zx::vmo dupe;
      ASSERT_OK(vmo_control_.duplicate(ZX_RIGHT_SAME_RIGHTS, &dupe));
      completer.ReplySuccess(fuchsia_mem::wire::Range{
          .vmo = std::move(dupe),
          .offset = 0,
          .size = kCtrlSize,
      });
    });

    mock_acpi_fidl_.SetGetBti([this](acpi::mock::Device::GetBtiRequestView rv,
                                     acpi::mock::Device::GetBtiCompleter::Sync& completer) {
      ASSERT_EQ(rv->index, 0);
      zx::bti out_bti;
      ASSERT_OK(acpi_bti_.duplicate(ZX_RIGHT_SAME_RIGHTS, &out_bti));
      completer.ReplySuccess(std::move(out_bti));
    });

    auto acpi_client = mock_acpi_fidl_.CreateClient(ns_loop_.dispatcher());
    ASSERT_OK(acpi_client.status_value());

    fake_root_->AddProtocol(ZX_PROTOCOL_ACPI, nullptr, nullptr, "acpi");

    auto dut = std::make_unique<PipeDevice>(fake_root_.get(), std::move(acpi_client.value()),
                                            test_loop_.dispatcher());
    ASSERT_OK(dut->Bind());
    dut_ = dut.release();

    {
      auto endpoints = fidl::Endpoints<fuchsia_hardware_goldfish_pipe::GoldfishPipe>::Create();

      dut_child_ = std::make_unique<PipeChildDevice>(dut_, test_loop_.dispatcher());
      binding_ =
          fidl::BindServer(test_loop_.dispatcher(), std::move(endpoints.server), dut_child_.get());
      EXPECT_TRUE(binding_.has_value());

      client_.Bind(std::move(endpoints.client), test_loop_.dispatcher());
    }
  }

  // |zxtest::Test|
  void TearDown() override {
    device_async_remove(fake_root_.get());
    mock_ddk::ReleaseFlaggedDevices(fake_root_.get());
  }

  std::unique_ptr<VmoMapping> MapControlRegisters() const {
    return std::make_unique<VmoMapping>(vmo_control_, /*size=*/sizeof(Registers), /*offset=*/0);
  }

  template <typename T>
  static void Flush(const T* t) {
    zx_cache_flush(t, sizeof(T), ZX_CACHE_FLUSH_DATA | ZX_CACHE_FLUSH_INVALIDATE);
  }

 protected:
  async::Loop ns_loop_;
  acpi::mock::Device mock_acpi_fidl_;

  std::shared_ptr<MockDevice> fake_root_;
  async::Loop test_loop_;
  PipeDevice* dut_;
  std::unique_ptr<PipeChildDevice> dut_child_;
  fidl::WireClient<fuchsia_hardware_goldfish_pipe::GoldfishPipe> client_;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_goldfish_pipe::GoldfishPipe>> binding_;

  zx::bti acpi_bti_;
  zx::vmo vmo_control_;
  zx::interrupt irq_;
};

TEST_F(PipeDeviceTest, Bind) {
  {
    auto mapped = MapControlRegisters();
    Registers* ctrl_regs = reinterpret_cast<Registers*>(mapped->ptr());
    ctrl_regs->version = kPipeMinDeviceVersion;
  }

  {
    auto mapped = MapControlRegisters();
    Registers* ctrl_regs = reinterpret_cast<Registers*>(mapped->ptr());
    Flush(ctrl_regs);

    zx_paddr_t signal_buffer = (static_cast<uint64_t>(ctrl_regs->signal_buffer_high) << 32u) |
                               (ctrl_regs->signal_buffer_low);
    ASSERT_NE(signal_buffer, 0u);

    uint32_t buffer_count = ctrl_regs->signal_buffer_count;
    ASSERT_EQ(buffer_count, kMaxSignalledPipes);

    zx_paddr_t open_buffer =
        (static_cast<uint64_t>(ctrl_regs->open_buffer_high) << 32u) | (ctrl_regs->open_buffer_low);
    ASSERT_NE(open_buffer, 0u);
  }
}

TEST_F(PipeDeviceTest, CreatePipe) {
  ASSERT_OK(dut_child_->Bind(kDefaultPipeDeviceProps, kDefaultPipeDeviceName));
  dut_child_.release();

  int32_t id = 0;
  zx::vmo vmo;
  client_->Create().Then([&](auto& result) {
    ASSERT_OK(result.status());
    id = result->value()->id;
    vmo = std::move(result->value()->vmo);
  });
  test_loop_.RunUntilIdle();

  ASSERT_NE(id, 0u);
  ASSERT_TRUE(vmo.is_valid());

  client_->Destroy(id).Then([](auto& result) { ASSERT_OK(result.status()); });
  test_loop_.RunUntilIdle();
}

TEST_F(PipeDeviceTest, Exec) {
  ASSERT_OK(dut_child_->Bind(kDefaultPipeDeviceProps, kDefaultPipeDeviceName));
  dut_child_.release();

  int32_t id = 0;
  zx::vmo vmo;
  client_->Create().Then([&](auto& result) {
    ASSERT_OK(result.status());
    id = result->value()->id;
    vmo = std::move(result->value()->vmo);
  });
  test_loop_.RunUntilIdle();

  ASSERT_NE(id, 0u);
  ASSERT_TRUE(vmo.is_valid());

  client_->Exec(id).Then([](auto& result) { ASSERT_OK(result.status()); });
  test_loop_.RunUntilIdle();

  {
    auto mapped = MapControlRegisters();
    Registers* ctrl_regs = reinterpret_cast<Registers*>(mapped->ptr());
    ASSERT_EQ(ctrl_regs->command, static_cast<uint32_t>(id));
  }

  client_->Destroy(id).Then([](auto& result) { ASSERT_OK(result.status()); });
  test_loop_.RunUntilIdle();
}

TEST_F(PipeDeviceTest, TransferObservedSignals) {
  ASSERT_OK(dut_child_->Bind(kDefaultPipeDeviceProps, kDefaultPipeDeviceName));
  dut_child_.release();

  int32_t id = 0;
  zx::vmo vmo;
  client_->Create().Then([&](auto& result) {
    ASSERT_OK(result.status());
    id = result->value()->id;
    vmo = std::move(result->value()->vmo);
  });
  test_loop_.RunUntilIdle();

  zx::event old_event, old_event_dup;
  ASSERT_OK(zx::event::create(0u, &old_event));
  ASSERT_OK(old_event.duplicate(ZX_RIGHT_SAME_RIGHTS, &old_event_dup));

  client_->SetEvent(id, std::move(old_event_dup)).Then([](auto& result) {
    ASSERT_OK(result.status());
  });
  test_loop_.RunUntilIdle();

  // Trigger signals on "old" event.
  old_event.signal(0u, fuchsia_hardware_goldfish::wire::kSignalReadable);

  zx::event new_event, new_event_dup;
  ASSERT_OK(zx::event::create(0u, &new_event));
  // Clear the target signal.
  ASSERT_OK(new_event.signal(fuchsia_hardware_goldfish::wire::kSignalReadable, 0u));
  ASSERT_OK(new_event.duplicate(ZX_RIGHT_SAME_RIGHTS, &new_event_dup));

  client_->SetEvent(id, std::move(new_event_dup)).Then([](auto& result) {
    ASSERT_OK(result.status());
  });
  test_loop_.RunUntilIdle();

  // Wait for `SIGNAL_READABLE` signal on the new event.
  zx_signals_t observed;
  ASSERT_OK(new_event.wait_one(fuchsia_hardware_goldfish::wire::kSignalReadable,
                               zx::time::infinite_past(), &observed));
}

TEST_F(PipeDeviceTest, GetBti) {
  ASSERT_OK(dut_child_->Bind(kDefaultPipeDeviceProps, kDefaultPipeDeviceName));
  dut_child_.release();

  zx::bti bti;
  client_->GetBti().Then([&](auto& result) {
    ASSERT_OK(result.status());
    bti = std::move(result->value()->bti);
  });
  test_loop_.RunUntilIdle();

  zx_info_bti_t goldfish_bti_info, acpi_bti_info;
  ASSERT_OK(
      bti.get_info(ZX_INFO_BTI, &goldfish_bti_info, sizeof(goldfish_bti_info), nullptr, nullptr));
  ASSERT_OK(
      acpi_bti_.get_info(ZX_INFO_BTI, &acpi_bti_info, sizeof(acpi_bti_info), nullptr, nullptr));

  ASSERT_FALSE(memcmp(&goldfish_bti_info, &acpi_bti_info, sizeof(zx_info_bti_t)));
}

TEST_F(PipeDeviceTest, ChildDevice) {
  // Test creating multiple child devices. Each child device can access the
  // GoldfishPipe FIDL protocol, and they should share the same parent device.

  auto child1 = std::make_unique<PipeChildDevice>(dut_, test_loop_.dispatcher());
  auto child2 = std::make_unique<PipeChildDevice>(dut_, test_loop_.dispatcher());

  const zx_device_str_prop_t kPropsChild1[] = {
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_VID,
                           bind_fuchsia_google_platform::BIND_PLATFORM_DEV_VID_GOOGLE),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_PID,
                           bind_fuchsia_goldfish_platform::BIND_PLATFORM_DEV_PID_GOLDFISH),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_DID, 0x01u),
  };
  constexpr const char* kDeviceNameChild1 = "goldfish-pipe-child1";
  ASSERT_OK(child1->Bind(kPropsChild1, kDeviceNameChild1));
  child1.release();

  const zx_device_str_prop_t kPropsChild2[] = {
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_VID,
                           bind_fuchsia_google_platform::BIND_PLATFORM_DEV_VID_GOOGLE),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_PID,
                           bind_fuchsia_goldfish_platform::BIND_PLATFORM_DEV_PID_GOLDFISH),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_DID, 0x02u),
  };
  constexpr const char* kDeviceNameChild2 = "goldfish-pipe-child2";
  ASSERT_OK(child2->Bind(kPropsChild2, kDeviceNameChild2));
  child2.release();

  int32_t id1 = 0;
  int32_t id2 = 0;
  client_->Create().Then([&](auto& result) {
    ASSERT_OK(result.status());
    id1 = result->value()->id;
  });
  client_->Create().Then([&](auto& result) {
    ASSERT_OK(result.status());
    id2 = result->value()->id;
  });
  test_loop_.RunUntilIdle();
  ASSERT_NE(id1, 0);
  ASSERT_NE(id2, 0);

  ASSERT_NE(id1, id2);
}

}  // namespace

}  // namespace goldfish
