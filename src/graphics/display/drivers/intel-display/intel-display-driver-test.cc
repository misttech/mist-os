// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-display/intel-display-driver.h"

#include <fidl/fuchsia.kernel/cpp/test_base.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fidl/fuchsia.system.state/cpp/test_base.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <fuchsia/hardware/intelgpucore/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/loop.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/driver_lifecycle.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fake-resource/resource.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zircon-internal/align.h>
#include <zircon/syscalls/resource.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/display/cpp/bind.h>
#include <bind/fuchsia/intel/platform/gpucore/cpp/bind.h>
#include <gtest/gtest.h>

#include "src/devices/pci/testing/pci_protocol_fake.h"
#include "src/graphics/display/drivers/intel-display/intel-display.h"
#include "src/graphics/display/drivers/intel-display/pci-ids.h"
#include "src/graphics/display/drivers/intel-display/testing/fake-buffer-collection.h"
#include "src/graphics/display/drivers/intel-display/testing/fake-framebuffer.h"
#include "src/graphics/display/drivers/intel-display/testing/mock-allocator.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/lib/testing/predicates/status.h"

namespace {

constexpr uint32_t kBytesPerRowDivisor = 1024;

}  // namespace

namespace intel_display {

namespace {

zx::resource CreateFakeRootResource() {
  zx::resource root;
  zx_status_t status = fake_root_resource_create(root.reset_and_get_address());
  ZX_ASSERT(status == ZX_OK);
  return root;
}

class FakeSystemStateTransition
    : public fidl::testing::TestBase<fuchsia_system_state::SystemStateTransition> {
 public:
  FakeSystemStateTransition() = default;
  ~FakeSystemStateTransition() override = default;

  // fidl::testing::TestBase:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ZX_PANIC("Not implemented: %s", name.c_str());
  }

  // fuchsia_system_state::SystemStateTransition:
  void GetTerminationSystemState(GetTerminationSystemStateCompleter::Sync& completer) override {
    completer.Reply({termination_system_state_});
  }

  void SetTerminationSystemState(fuchsia_system_state::SystemPowerState termination_system_state) {
    termination_system_state_ = termination_system_state;
  }

 private:
  fuchsia_system_state::SystemPowerState termination_system_state_ =
      fuchsia_system_state::SystemPowerState::kFullyOn;
};

class FakeFramebufferResource
    : public fidl::testing::TestBase<fuchsia_kernel::FramebufferResource> {
 public:
  // `root_resource` must outlive `FakeFramebufferResource`.
  explicit FakeFramebufferResource(zx::unowned_resource root_resource)
      : root_resource_(root_resource->borrow()) {}
  ~FakeFramebufferResource() override = default;

  // fidl::testing::TestBase:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ZX_PANIC("Not implemented: %s", name.c_str());
  }

  // fuchsia_kernel::FramebufferResource:
  void Get(GetCompleter::Sync& completer) override {
    zx::resource framebuffer_child;
    std::array<char, ZX_MAX_NAME_LEN> child_name = {"child"};
    zx_status_t status = zx_resource_create(root_resource_->get(), ZX_RSRC_SYSTEM_FRAMEBUFFER_BASE,
                                            0, 16, child_name.data(), child_name.size(),
                                            framebuffer_child.reset_and_get_address());
    ZX_ASSERT(status == ZX_OK);
    completer.Reply(std::move(framebuffer_child));
  }

 private:
  zx::unowned_resource root_resource_;
};

class FakeMmioResource : public fidl::testing::TestBase<fuchsia_kernel::MmioResource> {
 public:
  // `root_resource` must outlive `FakeFramebufferResource`.
  explicit FakeMmioResource(zx::unowned_resource root_resource)
      : root_resource_(root_resource->borrow()) {}
  ~FakeMmioResource() override = default;

  // fidl::testing::TestBase:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ZX_PANIC("Not implemented: %s", name.c_str());
  }

  // fuchsia_kernel::FramebufferResource:
  void Get(GetCompleter::Sync& completer) override {
    zx::resource mmio_child;
    std::array<char, ZX_MAX_NAME_LEN> child_name = {"child"};
    zx_status_t status =
        zx_resource_create(root_resource_->get(), ZX_RSRC_KIND_MMIO, 16, 32, child_name.data(),
                           child_name.size(), mmio_child.reset_and_get_address());
    ZX_ASSERT(status == ZX_OK);
    completer.Reply(std::move(mmio_child));
  }

 private:
  zx::unowned_resource root_resource_;
};

class FakeIoportResource : public fidl::testing::TestBase<fuchsia_kernel::IoportResource> {
 public:
  // `root_resource` must outlive `FakeFramebufferResource`.
  explicit FakeIoportResource(zx::unowned_resource root_resource)
      : root_resource_(root_resource->borrow()) {}
  ~FakeIoportResource() override = default;

  // fidl::testing::TestBase:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ZX_PANIC("Not implemented: %s", name.c_str());
  }

  // fuchsia_kernel::FramebufferResource:
  void Get(GetCompleter::Sync& completer) override {
    zx::resource ioport_child;
    std::array<char, ZX_MAX_NAME_LEN> child_name = {"child"};
    zx_status_t status =
        zx_resource_create(root_resource_->get(), ZX_RSRC_KIND_IOPORT, 32, 64, child_name.data(),
                           child_name.size(), ioport_child.reset_and_get_address());
    ZX_ASSERT(status == ZX_OK);
    completer.Reply(std::move(ioport_child));
  }

 private:
  zx::unowned_resource root_resource_;
};

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
class IntegrationTest : public ::testing::Test {
 public:
  void SetUp() override {
    SetUpFakeDevices();
    SetUpEnvironment();
  }

  void TearDown() override {
    driver_.reset();
    test_environment_.reset();
    node_server_.reset();
    driver_runtime_.ShutdownAllDispatchers(nullptr);
  }

  MockAllocator* sysmem() { return &sysmem_; }

 protected:
  fdf_testing::DriverRuntime driver_runtime_;
  fdf::UnownedSynchronizedDispatcher pci_dispatcher_ = driver_runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = driver_runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher display_dispatcher_ =
      driver_runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher_->async_dispatcher(), std::in_place, std::string("root")};
  async_patterns::TestDispatcherBound<fdf_testing::internal::TestEnvironment> test_environment_{
      env_dispatcher_->async_dispatcher(), std::in_place};
  async_patterns::TestDispatcherBound<fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>>
      driver_{display_dispatcher_->async_dispatcher(), std::in_place};

  pci::FakePciProtocol pci_;
  MockAllocator sysmem_{env_dispatcher_->async_dispatcher()};

  zx::resource fake_root_resource_ = CreateFakeRootResource();
  FakeFramebufferResource fake_framebuffer_resource_{fake_root_resource_.borrow()};
  FakeMmioResource fake_mmio_resource_{fake_root_resource_.borrow()};
  FakeIoportResource fake_ioport_resource_{fake_root_resource_.borrow()};
  FakeSystemStateTransition fake_system_state_transition_;

  fuchsia_driver_framework::DriverStartArgs start_args_;

 private:
 protected:
  void SetUpFakeFramebuffer();
  void SetUpFakeSysmem();
  void SetUpFakePci();
  void SetUpFakeDevices();
  void SetUpEnvironment();
};

void IntegrationTest::SetUpFakeFramebuffer() { fake_framebuffer::SetFramebuffer({}); }

void IntegrationTest::SetUpFakeSysmem() {
  sysmem_.SetNewBufferCollectionConfig({
      .cpu_domain_supported = false,
      .ram_domain_supported = true,
      .inaccessible_domain_supported = false,
      .bytes_per_row_divisor = kBytesPerRowDivisor,
      .format_modifier = fuchsia_images2::wire::PixelFormatModifier::kLinear,
  });
}

void IntegrationTest::SetUpFakePci() {
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
}

void IntegrationTest::SetUpFakeDevices() {
  SetUpFakeFramebuffer();
  SetUpFakeSysmem();
  SetUpFakePci();
}

void IntegrationTest::SetUpEnvironment() {
  zx::result create_start_args_zx_result =
      node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
  ASSERT_TRUE(create_start_args_zx_result.is_ok());

  auto [start_args, incoming_directory_server, outgoing_directory_client] =
      std::move(create_start_args_zx_result).value();
  start_args_ = std::move(start_args);

  zx::result<> add_sysmem_result =
      test_environment_.SyncCall([&](fdf_testing::internal::TestEnvironment* env) {
        return env->incoming_directory()
            .component()
            .AddUnmanagedProtocol<fuchsia_sysmem2::Allocator>(
                sysmem_.bind_handler(env_dispatcher_->async_dispatcher()));
      });
  ASSERT_OK(add_sysmem_result);

  zx::result<> add_pci_result =
      test_environment_.SyncCall([&](fdf_testing::internal::TestEnvironment* env) {
        return env->incoming_directory().AddService<fuchsia_hardware_pci::Service>(
            fuchsia_hardware_pci::Service::InstanceHandler(
                {.device = pci_.bind_handler(pci_dispatcher_->async_dispatcher())}),
            "pci");
      });
  ASSERT_OK(add_pci_result);

  zx::result<> add_framebuffer_resource_result =
      test_environment_.SyncCall([&](fdf_testing::internal::TestEnvironment* env) {
        return env->incoming_directory()
            .component()
            .AddUnmanagedProtocol<fuchsia_kernel::FramebufferResource>(
                fake_framebuffer_resource_.bind_handler(env_dispatcher_->async_dispatcher()));
      });
  ASSERT_OK(add_framebuffer_resource_result);

  zx::result<> add_mmio_resource_result =
      test_environment_.SyncCall([&](fdf_testing::internal::TestEnvironment* env) {
        return env->incoming_directory()
            .component()
            .AddUnmanagedProtocol<fuchsia_kernel::MmioResource>(
                fake_mmio_resource_.bind_handler(env_dispatcher_->async_dispatcher()));
      });
  ASSERT_OK(add_mmio_resource_result);

  zx::result<> add_ioport_resource_result =
      test_environment_.SyncCall([&](fdf_testing::internal::TestEnvironment* env) {
        return env->incoming_directory()
            .component()
            .AddUnmanagedProtocol<fuchsia_kernel::IoportResource>(
                fake_ioport_resource_.bind_handler(env_dispatcher_->async_dispatcher()));
      });
  ASSERT_OK(add_ioport_resource_result);

  zx::result<> add_system_state_transition_result =
      test_environment_.SyncCall([&](fdf_testing::internal::TestEnvironment* env) {
        return env->incoming_directory()
            .component()
            .AddUnmanagedProtocol<fuchsia_system_state::SystemStateTransition>(
                fake_system_state_transition_.bind_handler(env_dispatcher_->async_dispatcher()));
      });
  ASSERT_OK(add_system_state_transition_result);

  zx::result<> serve_result = test_environment_.SyncCall(
      &fdf_testing::internal::TestEnvironment::Initialize, std::move(incoming_directory_server));
  ASSERT_OK(serve_result);
}

struct DeviceNode {
  std::string name;
  std::vector<fuchsia_driver_framework::NodeProperty> properties;
};

bool IsDisplayEngineBanjoNode(const DeviceNode& node) {
  const std::vector<fuchsia_driver_framework::NodeProperty>& properties = node.properties;
  return properties.end() !=
         std::find_if(properties.begin(), properties.end(),
                      [](const fuchsia_driver_framework::NodeProperty& property) {
                        if (!property.key().string_value().has_value())
                          return false;
                        if (!property.value().int_value().has_value())
                          return false;
                        return property.key().string_value().value() == bind_fuchsia::PROTOCOL &&
                               property.value().int_value().value() ==
                                   bind_fuchsia_display::BIND_PROTOCOL_ENGINE;
                      });
}

bool IsIntelGpuCoreNode(const DeviceNode& node) {
  const std::vector<fuchsia_driver_framework::NodeProperty>& properties = node.properties;
  return properties.end() !=
         std::find_if(properties.begin(), properties.end(),
                      [](const fuchsia_driver_framework::NodeProperty& property) {
                        if (!property.key().string_value().has_value())
                          return false;
                        if (!property.value().int_value().has_value())
                          return false;
                        return property.key().string_value().value() == bind_fuchsia::PROTOCOL &&
                               property.value().int_value().value() ==
                                   bind_fuchsia_intel_platform_gpucore::BIND_PROTOCOL_DEVICE;
                      });
}

TEST_F(IntegrationTest, BindAndInit) {
  zx::result<> start_result = driver_runtime_.RunToCompletion(
      driver_.SyncCall(&fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>::Start,
                       (std::move(start_args_))));
  ASSERT_OK(start_result);

  std::vector<DeviceNode> nodes = node_server_.SyncCall([&](fdf_testing::TestNode* root) {
    std::vector<DeviceNode> nodes;
    for (auto& [name, node] : root->children()) {
      nodes.push_back({
          .name = name,
          .properties = node.GetProperties(),
      });
    }
    return nodes;
  });

  // There should be two published node: one "intel-display-controller" node
  // and a child "intel-gpu-core" node.
  ASSERT_EQ(nodes.size(), 2u);

  auto display_engine_banjo_node_it =
      std::find_if(nodes.begin(), nodes.end(), IsDisplayEngineBanjoNode);
  ASSERT_NE(display_engine_banjo_node_it, nodes.end());
  FDF_LOG(INFO, "Display controller banjo node is: %s", display_engine_banjo_node_it->name.c_str());

  auto intel_gpu_core_node_it = std::find_if(nodes.begin(), nodes.end(), IsIntelGpuCoreNode);
  ASSERT_NE(intel_gpu_core_node_it, nodes.end());
  FDF_LOG(INFO, "Intel GPU node is: %s", display_engine_banjo_node_it->name.c_str());

  ASSERT_NE(display_engine_banjo_node_it, intel_gpu_core_node_it);

  zx::result<> stop_result = driver_runtime_.RunToCompletion(
      driver_.SyncCall(&fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>::PrepareStop));
  EXPECT_OK(stop_result);
}

// Tests that the device can initialize even if bootloader framebuffer information is not available
// and global GTT allocations start at offset 0.
TEST_F(IntegrationTest, InitFailsIfBootloaderGetInfoFails) {
  fake_framebuffer::SetFramebuffer({.status = ZX_ERR_INVALID_ARGS});

  zx::result<> start_result = driver_runtime_.RunToCompletion(
      driver_.SyncCall(&fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>::Start,
                       (std::move(start_args_))));
  ASSERT_OK(start_result);

  zx::result<ddk::AnyProtocol> gpu_protocol_result =
      driver_.SyncCall([&](fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>* driver) {
        return (*driver)->GetProtocol(ZX_PROTOCOL_INTEL_GPU_CORE);
      });
  ASSERT_OK(gpu_protocol_result);
  ddk::AnyProtocol gpu_protocol = std::move(gpu_protocol_result).value();
  ddk::IntelGpuCoreProtocolClient gpu(
      reinterpret_cast<const intel_gpu_core_protocol_t*>(&gpu_protocol));

  uint64_t addr;

  zx_status_t alloc_status = gpu.GttAlloc(1, &addr);
  EXPECT_OK(alloc_status);
  EXPECT_EQ(0u, addr);

  zx::result<> stop_result = driver_runtime_.RunToCompletion(
      driver_.SyncCall(&fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>::PrepareStop));
  EXPECT_OK(stop_result);
}

// TODO(https://fxbug.dev/42166779): Add tests for DisplayPort display enumeration,
// covering the following cases:
//   - Display found during start up but not already powered.
//   - Display found during start up but already powered up.
//   - Display added and removed in a hotplug event.
// TODO(https://fxbug.dev/42167311): Add test for HDMI display enumeration.
// TODO(https://fxbug.dev/42167312): Add test for DVI display enumeration.

TEST_F(IntegrationTest, GttAllocationDoesNotOverlapBootloaderFramebuffer) {
  constexpr uint32_t kStride = 1920;
  constexpr uint32_t kHeight = 1080;
  fake_framebuffer::SetFramebuffer({
      .format = ZBI_PIXEL_FORMAT_RGB_888,
      .width = kStride,
      .height = kHeight,
      .stride = kStride,
  });

  zx::result<> start_result = driver_runtime_.RunToCompletion(
      driver_.SyncCall(&fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>::Start,
                       (std::move(start_args_))));
  ASSERT_OK(start_result);

  zx::result<ddk::AnyProtocol> gpu_protocol_result =
      driver_.SyncCall([&](fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>* driver) {
        return (*driver)->GetProtocol(ZX_PROTOCOL_INTEL_GPU_CORE);
      });
  ASSERT_OK(gpu_protocol_result);
  ddk::AnyProtocol gpu_protocol = std::move(gpu_protocol_result).value();
  ddk::IntelGpuCoreProtocolClient gpu(
      reinterpret_cast<const intel_gpu_core_protocol_t*>(&gpu_protocol));

  uint64_t addr;
  zx_status_t alloc_status = gpu.GttAlloc(1, &addr);
  EXPECT_OK(alloc_status);
  EXPECT_EQ(ZX_ROUNDUP(kHeight * kStride * 3, PAGE_SIZE), addr);

  zx::result<> stop_result = driver_runtime_.RunToCompletion(
      driver_.SyncCall(&fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>::PrepareStop));
  EXPECT_OK(stop_result);
}

TEST_F(IntegrationTest, SysmemImport) {
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

  zx::result<> start_result = driver_runtime_.RunToCompletion(
      driver_.SyncCall(&fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>::Start,
                       (std::move(start_args_))));
  ASSERT_OK(start_result);

  zx::result<ddk::AnyProtocol> display_protocol_result =
      driver_.SyncCall([&](fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>* driver) {
        return (*driver)->GetProtocol(ZX_PROTOCOL_DISPLAY_ENGINE);
      });
  ASSERT_OK(display_protocol_result);
  ddk::AnyProtocol display_protocol = std::move(display_protocol_result).value();
  ddk::DisplayEngineProtocolClient display(
      reinterpret_cast<const display_engine_protocol_t*>(&display_protocol));

  // Import buffer collection.
  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  auto [token_client, token_server] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();

  zx_status_t import_status = display.ImportBufferCollection(kBanjoBufferCollectionId,
                                                             std::move(token_client).TakeChannel());
  ASSERT_OK(import_status);

  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  zx_status_t set_constraints_status =
      display.SetBufferCollectionConstraints(&kDisplayUsage, kBanjoBufferCollectionId);
  ASSERT_OK(set_constraints_status);

  MockAllocator& allocator = *sysmem();
  driver_runtime_.RunUntil([&] {
    FakeBufferCollection* collection = allocator.GetMostRecentBufferCollection();
    return collection && collection->HasConstraints();
  });

  static constexpr image_metadata_t kDisplayImageMetadata = {
      .width = kImageWidth,
      .height = kImageHeight,
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  uint64_t image_handle = 0;
  zx_status_t import_image_status = display.ImportImage(
      &kDisplayImageMetadata, kBanjoBufferCollectionId, /*index=*/0, &image_handle);
  ASSERT_OK(import_image_status);

  uint64_t bytes_per_row =
      driver_.SyncCall([&](fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>* driver) {
        const GttRegion& region = (*driver)->controller()->SetupGttImage(
            kDisplayImageMetadata, image_handle, COORDINATE_TRANSFORMATION_IDENTITY);
        return region.bytes_per_row();
      });
  EXPECT_LT(kDisplayImageMetadata.width * 4, kBytesPerRowDivisor);
  EXPECT_EQ(kBytesPerRowDivisor, bytes_per_row);

  display.ReleaseImage(image_handle);

  zx::result<> stop_result = driver_runtime_.RunToCompletion(
      driver_.SyncCall(&fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>::PrepareStop));
  EXPECT_OK(stop_result);
}

TEST_F(IntegrationTest, SysmemRotated) {
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

  zx::result<> start_result = driver_runtime_.RunToCompletion(
      driver_.SyncCall(&fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>::Start,
                       (std::move(start_args_))));
  ASSERT_OK(start_result);

  zx::result<ddk::AnyProtocol> display_protocol_result =
      driver_.SyncCall([&](fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>* driver) {
        return (*driver)->GetProtocol(ZX_PROTOCOL_DISPLAY_ENGINE);
      });
  ASSERT_OK(display_protocol_result);
  ddk::AnyProtocol display_protocol = std::move(display_protocol_result).value();
  ddk::DisplayEngineProtocolClient display(
      reinterpret_cast<const display_engine_protocol_t*>(&display_protocol));

  // Import buffer collection.
  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  auto [token_client, token_server] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();

  zx_status_t import_status = display.ImportBufferCollection(kBanjoBufferCollectionId,
                                                             std::move(token_client).TakeChannel());
  ASSERT_OK(import_status);

  static constexpr image_buffer_usage_t kTiledDisplayUsage = {
      // Must be y or yf tiled so rotation is allowed.
      .tiling_type = IMAGE_TILING_TYPE_Y_LEGACY_TILED,
  };
  zx_status_t set_constraints_status =
      display.SetBufferCollectionConstraints(&kTiledDisplayUsage, kBanjoBufferCollectionId);
  ASSERT_OK(set_constraints_status);

  MockAllocator& allocator = *sysmem();
  driver_runtime_.RunUntil([&] {
    FakeBufferCollection* collection = allocator.GetMostRecentBufferCollection();
    return collection && collection->HasConstraints();
  });

  static constexpr image_metadata_t kDisplayImageMetadata = {
      .width = kImageWidth,
      .height = kImageHeight,
      .tiling_type = IMAGE_TILING_TYPE_Y_LEGACY_TILED,
  };
  uint64_t image_handle = 0;
  zx_status_t import_image_status = display.ImportImage(
      &kDisplayImageMetadata, kBanjoBufferCollectionId, /*index=*/0, &image_handle);
  ASSERT_OK(import_image_status);

  // Check that rotating the image doesn't hang.
  uint64_t bytes_per_row =
      driver_.SyncCall([&](fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>* driver) {
        const GttRegion& region = (*driver)->controller()->SetupGttImage(
            kDisplayImageMetadata, image_handle, COORDINATE_TRANSFORMATION_ROTATE_CCW_90);
        return region.bytes_per_row();
      });
  EXPECT_LT(kDisplayImageMetadata.width * 4, kBytesPerRowDivisor);
  EXPECT_EQ(kBytesPerRowDivisor, bytes_per_row);

  display.ReleaseImage(image_handle);

  zx::result<> stop_result = driver_runtime_.RunToCompletion(
      driver_.SyncCall(&fdf_testing::internal::DriverUnderTest<IntelDisplayDriver>::PrepareStop));
  EXPECT_OK(stop_result);
}

}  // namespace

}  // namespace intel_display
