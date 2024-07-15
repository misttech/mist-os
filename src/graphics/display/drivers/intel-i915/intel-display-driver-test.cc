// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-i915/intel-display-driver.h"

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <fuchsia/hardware/intelgpucore/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/async-loop/testing/cpp/real_loop.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zircon-internal/align.h>

#include <gtest/gtest.h>

#include "src/devices/pci/testing/pci_protocol_fake.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/drivers/intel-i915/intel-i915.h"
#include "src/graphics/display/drivers/intel-i915/pci-ids.h"
#include "src/graphics/display/drivers/intel-i915/testing/fake-buffer-collection.h"
#include "src/graphics/display/drivers/intel-i915/testing/fake-framebuffer.h"
#include "src/graphics/display/drivers/intel-i915/testing/mock-allocator.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/lib/testing/predicates/status.h"

namespace {

constexpr uint32_t kBytesPerRowDivisor = 1024;

}  // namespace

namespace i915 {

namespace {

class IntegrationTest : public ::testing::Test {
 public:
  void SetUp() final {
    fake_framebuffer::SetFramebuffer({});

    sysmem_.SetNewBufferCollectionConfig({
        .cpu_domain_supported = false,
        .ram_domain_supported = true,
        .inaccessible_domain_supported = false,
        .bytes_per_row_divisor = kBytesPerRowDivisor,
        .format_modifier = fuchsia_images2::wire::PixelFormatModifier::kLinear,
    });

    pci_.CreateBar(0u, std::numeric_limits<uint32_t>::max(), /*is_mmio=*/true);
    pci_.AddLegacyInterrupt();

    // This configures the "GMCH Graphics Control" register to report 2MB for the available GTT
    // Graphics Memory. All other bits of this register are set to zero and should get populated as
    // required for the tests below.
    pci_.PciWriteConfig16(registers::GmchGfxControl::kAddr, 0x40);

    constexpr uint16_t kIntelVendorId = 0x8086;
    pci_.SetDeviceInfo({
        .vendor_id = kIntelVendorId,
        .device_id = kTestDeviceDid,
    });

    parent_ = MockDevice::FakeRootParent();
    parent_->AddNsProtocol<fuchsia_sysmem2::Allocator>(
        sysmem_.bind_handler(env_dispatcher_->async_dispatcher()));

    zx::result<> add_service_result =
        outgoing_.SyncCall([this](component::OutgoingDirectory* outgoing) {
          return outgoing->AddService<fuchsia_hardware_pci::Service>(
              fuchsia_hardware_pci::Service::InstanceHandler(
                  {.device = pci_.bind_handler(pci_loop_.dispatcher())}));
        });
    ASSERT_EQ(add_service_result.status_value(), ZX_OK);

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(endpoints.status_value(), ZX_OK);

    zx::result<> serve_result =
        outgoing_.SyncCall(&component::OutgoingDirectory::Serve, std::move(endpoints->server));
    ASSERT_EQ(serve_result.status_value(), ZX_OK);

    parent_->AddFidlService(fuchsia_hardware_pci::Service::Name, std::move(endpoints->client),
                            "pci");
    pci_loop_.StartThread("pci-fidl-server-thread");
  }

  void TearDown() override {
    outgoing_.reset();
    pci_loop_.Shutdown();
    parent_ = nullptr;

    mock_ddk::GetDriverRuntime()->ShutdownAllDispatchers(/*dut_initial_dispatcher=*/nullptr);
  }

  MockDevice* parent() const { return parent_.get(); }

  MockAllocator* sysmem() { return &sysmem_; }

 protected:
  std::shared_ptr<fdf_testing::DriverRuntime> driver_runtime_ = mock_ddk::GetDriverRuntime();
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = driver_runtime_->StartBackgroundDispatcher();

  async::Loop pci_loop_{&kAsyncLoopConfigNeverAttachToThread};
  // Emulated parent protocols.
  pci::FakePciProtocol pci_;

  MockAllocator sysmem_{env_dispatcher_->async_dispatcher()};
  async_patterns::TestDispatcherBound<component::OutgoingDirectory> outgoing_{
      env_dispatcher_->async_dispatcher(), std::in_place, async_patterns::PassDispatcher};

  // mock-ddk parent device of the Controller under test.
  std::shared_ptr<MockDevice> parent_;
};

// Tests that DDK basic DDK lifecycle hooks function as expected.
TEST_F(IntegrationTest, BindAndInit) {
  async_dispatcher_t* current_dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
  zx::result<> create_status = zx::error(ZX_ERR_INTERNAL);
  async::PostTask(current_dispatcher,
                  [&] { create_status = IntelDisplayDriver::Create(parent()); });
  driver_runtime_->RunUntilIdle();
  ASSERT_OK(create_status.status_value());

  // There should be two published devices: one "intel_i915" device rooted at |parent()|, and a
  // grandchild "intel-gpu-core" device.
  ASSERT_EQ(1u, parent()->child_count());
  auto dev = parent()->GetLatestChild();
  ASSERT_EQ(2u, dev->child_count());

  // Perform the async initialization and wait for a response.
  async::PostTask(current_dispatcher, [&] { dev->InitOp(); });
  driver_runtime_->PerformBlockingWork([&] { EXPECT_EQ(ZX_OK, dev->WaitUntilInitReplyCalled()); });

  // Unbind the device and ensure it completes synchronously.
  async::PostTask(current_dispatcher, [&] { dev->UnbindOp(); });
  driver_runtime_->RunUntil([&] { return dev->UnbindReplyCalled(); });

  async::PostTask(current_dispatcher, [&] { mock_ddk::ReleaseFlaggedDevices(parent()); });
  driver_runtime_->RunUntilIdle();
  EXPECT_EQ(0u, dev->child_count());
}

// Tests that the device can initialize even if bootloader framebuffer information is not available
// and global GTT allocations start at offset 0.
TEST_F(IntegrationTest, InitFailsIfBootloaderGetInfoFails) {
  fake_framebuffer::SetFramebuffer({.status = ZX_ERR_INVALID_ARGS});

  async_dispatcher_t* current_dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();

  zx::result<> create_status = zx::error(ZX_ERR_INTERNAL);
  async::PostTask(current_dispatcher,
                  [&] { create_status = IntelDisplayDriver::Create(parent()); });
  driver_runtime_->RunUntilIdle();
  ASSERT_OK(create_status.status_value());

  auto dev = parent()->GetLatestChild();
  IntelDisplayDriver* intel_display_driver = dev->GetDeviceContext<IntelDisplayDriver>();
  Controller* controller = intel_display_driver->controller();

  uint64_t addr;
  EXPECT_EQ(ZX_OK, controller->IntelGpuCoreGttAlloc(1, &addr));
  EXPECT_EQ(0u, addr);
}

// TODO(https://fxbug.dev/42166779): Add tests for DisplayPort display enumeration by InitOp,
// covering the following cases:
//   - Display found during start up but not already powered.
//   - Display found during start up but already powered up.
//   - Display added and removed in a hotplug event.
// TODO(https://fxbug.dev/42167311): Add test for HDMI display enumeration by InitOp.
// TODO(https://fxbug.dev/42167312): Add test for DVI display enumeration by InitOp.

TEST_F(IntegrationTest, GttAllocationDoesNotOverlapBootloaderFramebuffer) {
  constexpr uint32_t kStride = 1920;
  constexpr uint32_t kHeight = 1080;
  fake_framebuffer::SetFramebuffer({
      .format = ZBI_PIXEL_FORMAT_RGB_888,
      .width = kStride,
      .height = kHeight,
      .stride = kStride,
  });

  async_dispatcher_t* current_dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
  zx::result<> create_status = zx::error(ZX_ERR_INTERNAL);
  async::PostTask(current_dispatcher,
                  [&] { create_status = IntelDisplayDriver::Create(parent()); });
  driver_runtime_->RunUntilIdle();
  ASSERT_OK(create_status.status_value());

  // There should be two published devices: one "intel_i915" device rooted at |parent()|, and a
  // grandchild "intel-gpu-core" device.
  ASSERT_EQ(1u, parent()->child_count());
  auto dev = parent()->GetLatestChild();
  IntelDisplayDriver* intel_display_driver = dev->GetDeviceContext<IntelDisplayDriver>();
  Controller* controller = intel_display_driver->controller();

  uint64_t addr;
  EXPECT_EQ(ZX_OK, controller->IntelGpuCoreGttAlloc(1, &addr));
  EXPECT_EQ(ZX_ROUNDUP(kHeight * kStride * 3, PAGE_SIZE), addr);
}

TEST_F(IntegrationTest, SysmemImport) {
  async_dispatcher_t* current_dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
  zx::result<> create_status = zx::error(ZX_ERR_INTERNAL);
  async::PostTask(current_dispatcher,
                  [&] { create_status = IntelDisplayDriver::Create(parent()); });
  driver_runtime_->RunUntilIdle();
  ASSERT_OK(create_status.status_value());

  // There should be two published devices: one "intel_i915" device rooted at `parent()`, and a
  // grandchild "intel-gpu-core" device.
  ASSERT_EQ(1u, parent()->child_count());
  auto dev = parent()->GetLatestChild();
  IntelDisplayDriver* intel_display_driver = dev->GetDeviceContext<IntelDisplayDriver>();
  Controller* controller = intel_display_driver->controller();

  static constexpr int kImageWidth = 128;
  static constexpr int kImageHeight = 32;
  sysmem_.SetNewBufferCollectionConfig({
      .cpu_domain_supported = false,
      .ram_domain_supported = true,
      .inaccessible_domain_supported = false,

      .width_fallback_px = kImageWidth,
      .height_fallback_px = kImageHeight,
      .bytes_per_row_divisor = kBytesPerRowDivisor,
      .format_modifier = fuchsia_images2::wire::PixelFormatModifier::kLinear,
  });

  // Import buffer collection.
  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());
  EXPECT_OK(controller->DisplayControllerImplImportBufferCollection(
      kBanjoBufferCollectionId, token_endpoints->client.TakeChannel()));

  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(controller->DisplayControllerImplSetBufferCollectionConstraints(
      &kDisplayUsage, kBanjoBufferCollectionId));
  MockAllocator& allocator = *sysmem();
  driver_runtime_->RunUntil([&] {
    FakeBufferCollection* collection = allocator.GetMostRecentBufferCollection();
    return collection && collection->HasConstraints();
  });

  static constexpr image_metadata_t kDisplayImageMetadata = {
      .width = kImageWidth,
      .height = kImageHeight,
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  uint64_t image_handle = 0;

  EXPECT_OK(controller->DisplayControllerImplImportImage(&kDisplayImageMetadata,
                                                         kBanjoBufferCollectionId,
                                                         /*index=*/0, &image_handle));

  const GttRegion& region =
      controller->SetupGttImage(kDisplayImageMetadata, image_handle, FRAME_TRANSFORM_IDENTITY);
  EXPECT_LT(kDisplayImageMetadata.width * 4, kBytesPerRowDivisor);
  EXPECT_EQ(kBytesPerRowDivisor, region.bytes_per_row());
  controller->DisplayControllerImplReleaseImage(image_handle);
}

TEST_F(IntegrationTest, SysmemRotated) {
  async_dispatcher_t* current_dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
  zx::result<> create_status = zx::error(ZX_ERR_INTERNAL);
  async::PostTask(current_dispatcher,
                  [&] { create_status = IntelDisplayDriver::Create(parent()); });
  driver_runtime_->RunUntilIdle();
  ASSERT_OK(create_status.status_value());

  // There should be two published devices: one "intel_i915" device rooted at `parent()`, and a
  // grandchild "intel-gpu-core" device.
  ASSERT_EQ(1u, parent()->child_count());
  auto dev = parent()->GetLatestChild();
  IntelDisplayDriver* intel_display_driver = dev->GetDeviceContext<IntelDisplayDriver>();
  Controller* controller = intel_display_driver->controller();

  static constexpr int kImageWidth = 128;
  static constexpr int kImageHeight = 32;
  sysmem_.SetNewBufferCollectionConfig({
      .cpu_domain_supported = false,
      .ram_domain_supported = true,
      .inaccessible_domain_supported = false,

      .width_fallback_px = kImageWidth,
      .height_fallback_px = kImageHeight,
      .bytes_per_row_divisor = kBytesPerRowDivisor,
      .format_modifier = fuchsia_images2::wire::PixelFormatModifier::kIntelI915YTiled,
  });

  // Import buffer collection.
  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());
  EXPECT_OK(controller->DisplayControllerImplImportBufferCollection(
      kBanjoBufferCollectionId, token_endpoints->client.TakeChannel()));

  MockAllocator& allocator = *sysmem();
  driver_runtime_->RunUntil([&] {
    FakeBufferCollection* collection = allocator.GetMostRecentBufferCollection();
    return collection != nullptr;
  });
  FakeBufferCollection* collection = allocator.GetMostRecentBufferCollection();

  static constexpr image_buffer_usage_t kTiledDisplayUsage = {
      // Must be y or yf tiled so rotation is allowed.
      .tiling_type = IMAGE_TILING_TYPE_Y_LEGACY_TILED,
  };
  EXPECT_OK(controller->DisplayControllerImplSetBufferCollectionConstraints(
      &kTiledDisplayUsage, kBanjoBufferCollectionId));

  driver_runtime_->RunUntil([&] { return collection->HasConstraints(); });

  static constexpr image_metadata_t kTiledImageMetadata = {
      .width = kImageWidth,
      .height = kImageHeight,
      .tiling_type = IMAGE_TILING_TYPE_Y_LEGACY_TILED,
  };
  uint64_t image_handle = 0;
  driver_runtime_->PerformBlockingWork([&]() mutable {
    EXPECT_OK(controller->DisplayControllerImplImportImage(&kTiledImageMetadata,
                                                           kBanjoBufferCollectionId,
                                                           /*index=*/0, &image_handle));
  });

  // Check that rotating the image doesn't hang.
  const GttRegion& region =
      controller->SetupGttImage(kTiledImageMetadata, image_handle, FRAME_TRANSFORM_ROT_90);
  EXPECT_LT(kTiledImageMetadata.width * 4, kBytesPerRowDivisor);
  EXPECT_EQ(kBytesPerRowDivisor, region.bytes_per_row());
  controller->DisplayControllerImplReleaseImage(image_handle);
}

}  // namespace

}  // namespace i915
