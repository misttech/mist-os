// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.kernel/cpp/wire_test_base.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/fake-platform-device/cpp/fake-pdev.h>
#include <lib/driver/fake-resource/cpp/fake-resource.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/driver_lifecycle.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/zx/result.h>

#include <gtest/gtest.h>

#include "src/graphics/drivers/msd-arm-mali/config.h"
#include "src/graphics/drivers/msd-arm-mali/src/gpu_features.h"
#include "src/graphics/drivers/msd-arm-mali/src/registers.h"

enum InterruptIndex {
  kInterruptIndexJob = 0,
  kInterruptIndexMmu = 1,
  kInterruptIndexGpu = 2,
};
namespace {
std::unique_ptr<magma::RegisterIo::Hook> hook_s;
}  // namespace

// Overrides the implementation in msd_arm_device.cc
void InstallMaliRegisterIoHook(magma::RegisterIo* register_io) {
  if (hook_s) {
    register_io->InstallHook(std::move(hook_s));
  }
}

namespace {

class FakeInfoResource : public fidl::testing::WireTestBase<fuchsia_kernel::InfoResource> {
 public:
  FakeInfoResource() {}

  void Get(GetCompleter::Sync& completer) override {
    zx::resource resource;
    fake_root_resource_create(resource.reset_and_get_address());
    completer.Reply(std::move(resource));
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
};

class TestEnvironmentWrapper {
 public:
  fdf::DriverStartArgs Setup(fdf_fake::FakePDev::Config pdev_config) {
    zx::result start_args_result = node_.CreateStartArgsAndServe();
    EXPECT_EQ(ZX_OK, start_args_result.status_value());

    EXPECT_EQ(
        ZX_OK,
        env_.Initialize(std::move(start_args_result->incoming_directory_server)).status_value());

    pdev_.SetConfig(std::move(pdev_config));

    auto result = env_.incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
        pdev_.GetInstanceHandler(fdf::Dispatcher::GetCurrent()->async_dispatcher()), "pdev");
    EXPECT_EQ(ZX_OK, result.status_value());

    EXPECT_EQ(ZX_OK,
              env_.incoming_directory()
                  .component()
                  .AddProtocol<fuchsia_kernel::InfoResource>(std::make_unique<FakeInfoResource>())
                  .status_value());

    return std::move(start_args_result->start_args);
  }

 private:
  fdf_testing::TestNode node_{"root"};
  fdf_testing::internal::TestEnvironment env_;
  fdf_fake::FakePDev pdev_;
};

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
TEST(MsdArmDFv2, LoadDriver) {
  fdf_testing::DriverRuntime runtime;

  // This dispatcher is used by the test environment, and hosts the FakePDevFidl and incoming
  // directory.
  fdf::UnownedSynchronizedDispatcher test_env_dispatcher = runtime.StartBackgroundDispatcher();

  // Initialize MMIOs and IRQs needed by the device.
  zx::interrupt gpu_interrupt;
  zx::result<fdf::MmioBuffer> mmio_buffer;
  fdf_fake::FakePDev::Config config{.use_fake_bti = true, .use_fake_irq = true};
  {
    ASSERT_EQ(ZX_OK,
              zx::interrupt::create(zx::resource(0), 0, ZX_INTERRUPT_VIRTUAL, &gpu_interrupt));
    zx::interrupt dup_interrupt;
    ASSERT_EQ(ZX_OK, gpu_interrupt.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup_interrupt));
    config.irqs[kInterruptIndexGpu] = std::move(dup_interrupt);

    constexpr uint64_t kMmioSize = 0x100000;
    zx::vmo vmo;
    ASSERT_EQ(ZX_OK, zx::vmo::create(kMmioSize, 0, &vmo));
    zx::vmo dup_vmo;
    ASSERT_EQ(ZX_OK, vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup_vmo));
    mmio_buffer =
        fdf::MmioBuffer::Create(0, kMmioSize, std::move(dup_vmo), ZX_CACHE_POLICY_UNCACHED_DEVICE);
    ASSERT_EQ(ZX_OK, mmio_buffer.status_value());
    config.mmios[0] = fdf::PDev::MmioInfo{.size = kMmioSize, .vmo = std::move(vmo)};
  }

  async_patterns::TestDispatcherBound<TestEnvironmentWrapper> test_environment{
      test_env_dispatcher->async_dispatcher(), std::in_place};

  fdf::DriverStartArgs start_args =
      test_environment.SyncCall(&TestEnvironmentWrapper::Setup, std::move(config));

  class MaliHook : public magma::RegisterIo::Hook {
   public:
    MaliHook(fdf::MmioBuffer* mmio_buffer, zx::interrupt* gpu_interrupt)
        : mmio_buffer_(mmio_buffer), gpu_interrupt_(gpu_interrupt) {}
    void Write32(uint32_t val, uint32_t offset) override {
      if ((offset == registers::GpuCommand::kOffset) &&
          (val == registers::GpuCommand::kCmdSoftReset)) {
        // Mark that the reset has completed.
        auto irq_status = registers::GpuIrqFlags::GetStatus().FromValue(0);
        irq_status.set_reset_completed(1);
        irq_status.WriteTo(mmio_buffer_);
        gpu_interrupt_->trigger(0, zx::time_boot());
      }
    }

    virtual void Read32(uint32_t val, uint32_t offset) override {}

    virtual void Read64(uint64_t val, uint32_t offset) override {}

   private:
    fdf::MmioBuffer* mmio_buffer_;
    zx::interrupt* gpu_interrupt_;
  };
  hook_s = std::make_unique<MaliHook>(&*mmio_buffer, &gpu_interrupt);

  // Mark that shader cores are ready.
  {
    constexpr uint64_t kCoresEnabled = (1 << 2) - 1;
    constexpr uint32_t kShaderReadyOffset =
        static_cast<uint32_t>(registers::CoreReadyState::CoreType::kShader) +
        static_cast<uint32_t>(registers::CoreReadyState::StatusType::kReady);
    mmio_buffer->Write32(kCoresEnabled, kShaderReadyOffset);
    mmio_buffer->Write<uint32_t>(kCoresEnabled, GpuFeatures::kShaderPresentLowOffset);
  }
  {
    config::Config fake_config;
    fake_config.enable_suspend() = false;
    start_args.config(fake_config.ToVmo());
  }

  fdf_testing::internal::DriverUnderTest<> driver;

  zx::result start_result = runtime.RunToCompletion(driver.Start(std::move(start_args)));
  ASSERT_EQ(ZX_OK, start_result.status_value());

  // Hook ownership should have been taken by the driver.
  EXPECT_FALSE(hook_s);

  zx::result stop_result = runtime.RunToCompletion(driver.PrepareStop());
  ASSERT_EQ(ZX_OK, stop_result.status_value());
}

}  // namespace
