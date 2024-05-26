// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-i915/intel-i915.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire_test_base.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire_test_base.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <fuchsia/hardware/intelgpucore/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/async-loop/testing/cpp/real_loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/driver.h>
#include <lib/mmio-ptr/fake.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zircon-internal/align.h>
#include <lib/zx/vmar.h>

#include <vector>

#include <gtest/gtest.h>

#include "src/devices/pci/testing/pci_protocol_fake.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/drivers/intel-i915/pci-ids.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"
#include "src/lib/fsl/handles/object_info.h"

#define ASSERT_OK(x) ASSERT_EQ(ZX_OK, (x))
#define EXPECT_OK(x) EXPECT_EQ(ZX_OK, (x))

namespace {
constexpr uint32_t kBytesPerRowDivisor = 1024;
constexpr uint32_t kImageHeight = 32;

// Module-scope global data structure that acts as the data source for the zx_framebuffer_get_info
// implementation below.
struct Framebuffer {
  zx_status_t status = ZX_OK;
  uint32_t format = 0u;
  uint32_t width = 0u;
  uint32_t height = 0u;
  uint32_t stride = 0u;
};
std::mutex g_lock_;
Framebuffer g_framebuffer;

void SetFramebuffer(const Framebuffer& buffer) {
  std::lock_guard guard(g_lock_);
  g_framebuffer = buffer;
}

}  // namespace

zx_status_t zx_framebuffer_get_info(zx_handle_t resource, uint32_t* format, uint32_t* width,
                                    uint32_t* height, uint32_t* stride) {
  std::lock_guard guard(g_lock_);
  *format = g_framebuffer.format;
  *width = g_framebuffer.width;
  *height = g_framebuffer.height;
  *stride = g_framebuffer.stride;
  return g_framebuffer.status;
}

namespace i915 {

namespace {

// TODO(https://fxbug.dev/42072949): Consider creating and using a unified set of sysmem
// testing doubles instead of writing mocks for each display driver test.
class MockNoCpuBufferCollection
    : public fidl::testing::WireTestBase<fuchsia_sysmem2::BufferCollection> {
 public:
  void set_format_modifier(fuchsia_images2::wire::PixelFormatModifier format_modifier) {
    format_modifier_ = format_modifier;
  }

  bool set_constraints_called() const { return set_constraints_called_; }
  void SetConstraints(SetConstraintsRequestView request,
                      SetConstraintsCompleter::Sync& _completer) override {
    set_constraints_called_ = true;
    if (!request->has_constraints()) {
      return;
    }

    EXPECT_TRUE(
        !request->constraints().buffer_memory_constraints().has_inaccessible_domain_supported() ||
        !request->constraints().buffer_memory_constraints().inaccessible_domain_supported());
    EXPECT_TRUE(!request->constraints().buffer_memory_constraints().has_cpu_domain_supported() ||
                !request->constraints().buffer_memory_constraints().cpu_domain_supported());
    constraints_ = fidl::ToNatural(request->constraints());
  }

  void CheckAllBuffersAllocated(CheckAllBuffersAllocatedCompleter::Sync& completer) override {
    completer.Reply(fit::ok());
  }

  void WaitForAllBuffersAllocated(WaitForAllBuffersAllocatedCompleter::Sync& completer) override {
    auto info = fuchsia_sysmem2::wire::BufferCollectionInfo::Builder(arena_);

    for (size_t i = 0; i < constraints_.image_format_constraints()->size(); i++) {
      if (constraints_.image_format_constraints()->at(i).pixel_format_modifier() ==
          format_modifier_) {
        auto& constraints = constraints_.image_format_constraints()->at(i);
        constraints.bytes_per_row_divisor(kBytesPerRowDivisor);
        info.settings(fuchsia_sysmem2::wire::SingleBufferSettings::Builder(arena_)
                          .image_format_constraints(fidl::ToWire(arena_, constraints))
                          .Build());
        break;
      }
    }
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(kBytesPerRowDivisor * kImageHeight, 0, &vmo));
    info.buffers(std::vector{fuchsia_sysmem2::wire::VmoBuffer::Builder(arena_)
                                 .vmo(std::move(vmo))
                                 .vmo_usable_start(0)
                                 .Build()});
    auto response =
        fuchsia_sysmem2::wire::BufferCollectionWaitForAllBuffersAllocatedResponse::Builder(arena_)
            .buffer_collection_info(info.Build())
            .Build();
    completer.Reply(fit::ok(&response));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    EXPECT_TRUE(false);
  }

 private:
  fidl::Arena<fidl::kDefaultArenaInitialCapacity> arena_;
  bool set_constraints_called_ = false;
  fuchsia_images2::wire::PixelFormatModifier format_modifier_ =
      fuchsia_images2::wire::PixelFormatModifier::kLinear;
  fuchsia_sysmem2::BufferCollectionConstraints constraints_;
};

class MockAllocator : public fidl::testing::WireTestBase<fuchsia_sysmem2::Allocator> {
 public:
  explicit MockAllocator(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {
    EXPECT_TRUE(dispatcher_);
  }

  void BindSharedCollection(BindSharedCollectionRequestView request,
                            BindSharedCollectionCompleter::Sync& completer) override {
    const std::vector<fuchsia_images2::wire::PixelFormat> kPixelFormatTypes = {
        fuchsia_images2::wire::PixelFormat::kB8G8R8A8,
        fuchsia_images2::wire::PixelFormat::kR8G8B8A8};

    display::DriverBufferCollectionId buffer_collection_id = next_buffer_collection_id_++;
    active_buffer_collections_[buffer_collection_id] = {
        .token_client = std::move(request->token()),
        .mock_buffer_collection = std::make_unique<MockNoCpuBufferCollection>(),
    };
    most_recent_buffer_collection_ =
        active_buffer_collections_.at(buffer_collection_id).mock_buffer_collection.get();

    fidl::BindServer(
        dispatcher_, std::move(request->buffer_collection_request()),
        active_buffer_collections_[buffer_collection_id].mock_buffer_collection.get(),
        [this, buffer_collection_id](MockNoCpuBufferCollection*, fidl::UnbindInfo,
                                     fidl::ServerEnd<fuchsia_sysmem2::BufferCollection>) {
          inactive_buffer_collection_tokens_.push_back(
              std::move(active_buffer_collections_[buffer_collection_id].token_client));
          active_buffer_collections_.erase(buffer_collection_id);
        });
  }

  void SetDebugClientInfo(SetDebugClientInfoRequestView request,
                          SetDebugClientInfoCompleter::Sync& completer) override {
    EXPECT_EQ(request->name().get().find("intel-i915"), 0u);
  }

  // Returns the most recent created BufferCollection server.
  // This may go out of scope if the caller releases the BufferCollection.
  MockNoCpuBufferCollection* GetMostRecentBufferCollection() const {
    return most_recent_buffer_collection_;
  }

  std::vector<fidl::UnownedClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
  GetActiveBufferCollectionTokenClients() const {
    std::vector<fidl::UnownedClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
        unowned_token_clients;
    unowned_token_clients.reserve(active_buffer_collections_.size());

    for (const auto& kv : active_buffer_collections_) {
      unowned_token_clients.push_back(kv.second.token_client);
    }
    return unowned_token_clients;
  }

  std::vector<fidl::UnownedClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
  GetInactiveBufferCollectionTokenClients() const {
    std::vector<fidl::UnownedClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
        unowned_token_clients;
    unowned_token_clients.reserve(inactive_buffer_collection_tokens_.size());

    for (const auto& token : inactive_buffer_collection_tokens_) {
      unowned_token_clients.push_back(token);
    }
    return unowned_token_clients;
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    EXPECT_TRUE(false);
  }

 private:
  struct BufferCollection {
    fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken> token_client;
    std::unique_ptr<MockNoCpuBufferCollection> mock_buffer_collection;
  };

  MockNoCpuBufferCollection* most_recent_buffer_collection_ = nullptr;
  std::unordered_map<display::DriverBufferCollectionId, BufferCollection>
      active_buffer_collections_;
  std::vector<fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
      inactive_buffer_collection_tokens_;

  display::DriverBufferCollectionId next_buffer_collection_id_ =
      display::DriverBufferCollectionId(0);

  async_dispatcher_t* dispatcher_ = nullptr;
};

class IntegrationTest : public ::testing::Test, public loop_fixture::RealLoop {
 protected:
  IntegrationTest()
      : pci_loop_(&kAsyncLoopConfigNeverAttachToThread),
        sysmem_(dispatcher()),
        outgoing_(dispatcher()) {}

  void SetUp() final {
    SetFramebuffer({});

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

    parent_->AddNsProtocol<fuchsia_sysmem2::Allocator>(sysmem_.bind_handler(dispatcher()));

    zx::result service_result = outgoing_.AddService<fuchsia_hardware_pci::Service>(
        fuchsia_hardware_pci::Service::InstanceHandler(
            {.device = pci_.bind_handler(pci_loop_.dispatcher())}));
    ZX_ASSERT(service_result.is_ok());

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(endpoints.status_value(), ZX_OK);
    ASSERT_EQ(outgoing_.Serve(std::move(endpoints->server)).status_value(), ZX_OK);

    parent_->AddFidlService(fuchsia_hardware_pci::Service::Name, std::move(endpoints->client),
                            "pci");
    pci_loop_.StartThread("pci-fidl-server-thread");
  }

  void TearDown() override {
    loop().Shutdown();
    pci_loop_.Shutdown();

    parent_ = nullptr;
  }

  MockDevice* parent() const { return parent_.get(); }

  MockAllocator* sysmem() { return &sysmem_; }

 private:
  async::Loop pci_loop_;
  // Emulated parent protocols.
  pci::FakePciProtocol pci_;
  MockAllocator sysmem_;
  component::OutgoingDirectory outgoing_;

  // mock-ddk parent device of the Controller under test.
  std::shared_ptr<MockDevice> parent_;
};

// Test fixture for tests that only uses fake sysmem but doesn't have any
// other dependency, so that we won't need a fully-fledged device tree.
class FakeSysmemSingleThreadedTest : public testing::Test {
 public:
  FakeSysmemSingleThreadedTest()
      : loop_(&kAsyncLoopConfigAttachToCurrentThread),
        sysmem_(loop_.dispatcher()),
        display_(nullptr) {}

  void SetUp() override {
    auto [sysmem_client, sysmem_server] = fidl::Endpoints<fuchsia_sysmem2::Allocator>::Create();
    fidl::BindServer(loop_.dispatcher(), std::move(sysmem_server), &sysmem_);

    ASSERT_OK(display_.SetAndInitSysmemForTesting(fidl::WireSyncClient(std::move(sysmem_client))));
    EXPECT_OK(loop_.RunUntilIdle());
  }

  void TearDown() override {
    // Shutdown the loop before destroying the FakeSysmem and MockAllocator which
    // may still have pending callbacks.
    loop_.Shutdown();
  }

 protected:
  async::Loop loop_;

  MockAllocator sysmem_;
  Controller display_;
};

using ControllerWithFakeSysmemTest = FakeSysmemSingleThreadedTest;

TEST_F(ControllerWithFakeSysmemTest, ImportBufferCollection) {
  const MockAllocator& allocator = sysmem_;

  zx::result token1_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token1_endpoints.is_ok());
  zx::result token2_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token2_endpoints.is_ok());

  // Test ImportBufferCollection().
  constexpr display::DriverBufferCollectionId kValidBufferCollectionId(1);
  constexpr uint64_t kBanjoValidBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kValidBufferCollectionId);
  EXPECT_OK(display_.DisplayControllerImplImportBufferCollection(
      kBanjoValidBufferCollectionId, token1_endpoints->client.TakeChannel()));

  // `collection_id` must be unused.
  EXPECT_EQ(display_.DisplayControllerImplImportBufferCollection(
                kBanjoValidBufferCollectionId, token2_endpoints->client.TakeChannel()),
            ZX_ERR_ALREADY_EXISTS);

  loop_.RunUntilIdle();

  // Verify that the current buffer collection token is used.
  {
    auto active_buffer_token_clients = allocator.GetActiveBufferCollectionTokenClients();
    EXPECT_EQ(active_buffer_token_clients.size(), 1u);

    auto inactive_buffer_token_clients = allocator.GetInactiveBufferCollectionTokenClients();
    EXPECT_EQ(inactive_buffer_token_clients.size(), 0u);

    auto [client_koid, client_related_koid] =
        fsl::GetKoids(active_buffer_token_clients[0].channel()->get());
    auto [server_koid, server_related_koid] =
        fsl::GetKoids(token1_endpoints->server.channel().get());

    EXPECT_NE(client_koid, ZX_KOID_INVALID);
    EXPECT_NE(client_related_koid, ZX_KOID_INVALID);
    EXPECT_NE(server_koid, ZX_KOID_INVALID);
    EXPECT_NE(server_related_koid, ZX_KOID_INVALID);

    EXPECT_EQ(client_koid, server_related_koid);
    EXPECT_EQ(server_koid, client_related_koid);
  }

  // Test ReleaseBufferCollection().
  constexpr display::DriverBufferCollectionId kInvalidBufferCollectionId(2);
  constexpr uint64_t kBanjoInvalidBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kInvalidBufferCollectionId);
  EXPECT_EQ(display_.DisplayControllerImplReleaseBufferCollection(kBanjoInvalidBufferCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display_.DisplayControllerImplReleaseBufferCollection(kBanjoValidBufferCollectionId));

  loop_.RunUntilIdle();

  // Verify that the current buffer collection token is released.
  {
    auto active_buffer_token_clients = allocator.GetActiveBufferCollectionTokenClients();
    EXPECT_EQ(active_buffer_token_clients.size(), 0u);

    auto inactive_buffer_token_clients = allocator.GetInactiveBufferCollectionTokenClients();
    EXPECT_EQ(inactive_buffer_token_clients.size(), 1u);

    auto [client_koid, client_related_koid] =
        fsl::GetKoids(inactive_buffer_token_clients[0].channel()->get());
    auto [server_koid, server_related_koid] =
        fsl::GetKoids(token1_endpoints->server.channel().get());

    EXPECT_NE(client_koid, ZX_KOID_INVALID);
    EXPECT_NE(client_related_koid, ZX_KOID_INVALID);
    EXPECT_NE(server_koid, ZX_KOID_INVALID);
    EXPECT_NE(server_related_koid, ZX_KOID_INVALID);

    EXPECT_EQ(client_koid, server_related_koid);
    EXPECT_EQ(server_koid, client_related_koid);
  }
}

fdf::MmioBuffer MakeMmioBuffer(uint8_t* buffer, size_t size) {
  return fdf::MmioBuffer({
      .vaddr = FakeMmioPtr(buffer),
      .offset = 0,
      .size = size,
      .vmo = ZX_HANDLE_INVALID,
  });
}

TEST(IntelI915Display, ImportImage) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  loop.StartThread("fidl-loop");

  // Prepare fake sysmem.
  MockAllocator fake_sysmem(loop.dispatcher());
  auto [sysmem_client, sysmem_server] = fidl::Endpoints<fuchsia_sysmem2::Allocator>::Create();
  fidl::BindServer(loop.dispatcher(), std::move(sysmem_server), &fake_sysmem);

  // Prepare fake PCI.
  pci::FakePciProtocol fake_pci;
  ddk::Pci pci = fake_pci.SetUpFidlServer(loop);

  // Initialize display controller and sysmem allocator.
  Controller display(nullptr);
  ASSERT_OK(display.SetAndInitSysmemForTesting(fidl::WireSyncClient(std::move(sysmem_client))));

  // Initialize the GTT to the smallest allowed size (which is 2MB with the |gtt_size| bits of the
  // graphics control register set to 0x01.
  constexpr size_t kGraphicsTranslationTableSizeBytes = (1 << 21);
  ASSERT_OK(pci.WriteConfig16(registers::GmchGfxControl::kAddr,
                              registers::GmchGfxControl().set_gtt_size(0x01).reg_value()));
  auto buffer = std::make_unique<uint8_t[]>(kGraphicsTranslationTableSizeBytes);
  memset(buffer.get(), 0, kGraphicsTranslationTableSizeBytes);
  fdf::MmioBuffer mmio = MakeMmioBuffer(buffer.get(), kGraphicsTranslationTableSizeBytes);
  ASSERT_OK(display.InitGttForTesting(pci, std::move(mmio), /*fb_offset=*/0));

  // Import buffer collection.
  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());
  EXPECT_OK(display.DisplayControllerImplImportBufferCollection(
      kBanjoBufferCollectionId, token_endpoints->client.TakeChannel()));

  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(display.DisplayControllerImplSetBufferCollectionConstraints(&kDisplayUsage,
                                                                        kBanjoBufferCollectionId));

  // Invalid import: bad collection id
  static constexpr image_metadata_t kDisplayImageMetadata = {
      .width = 32,
      .height = 32,
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  static constexpr uint64_t kBanjoInvalidCollectionId = 100;
  uint64_t image_handle = 0;
  EXPECT_EQ(display.DisplayControllerImplImportImage(&kDisplayImageMetadata,
                                                     kBanjoInvalidCollectionId, 0, &image_handle),
            ZX_ERR_NOT_FOUND);

  // Invalid import: bad index
  static constexpr uint32_t kInvalidIndex = 100;
  image_handle = 0;
  EXPECT_EQ(display.DisplayControllerImplImportImage(
                &kDisplayImageMetadata, kBanjoBufferCollectionId, kInvalidIndex, &image_handle),
            ZX_ERR_OUT_OF_RANGE);

  // Invalid import: bad type
  static constexpr image_metadata_t kInvalidTilingTypeMetadata = {
      .width = 32,
      .height = 32,
      .tiling_type = IMAGE_TILING_TYPE_CAPTURE,
  };
  EXPECT_EQ(display.DisplayControllerImplImportImage(&kInvalidTilingTypeMetadata,
                                                     kBanjoBufferCollectionId,
                                                     /*index=*/0, &image_handle),
            ZX_ERR_INVALID_ARGS);

  // Valid import
  image_handle = 0;
  EXPECT_OK(display.DisplayControllerImplImportImage(&kDisplayImageMetadata,
                                                     kBanjoBufferCollectionId, 0, &image_handle));
  EXPECT_NE(image_handle, 0u);

  display.DisplayControllerImplReleaseImage(image_handle);

  // Release buffer collection.
  EXPECT_OK(display.DisplayControllerImplReleaseBufferCollection(kBanjoBufferCollectionId));

  // Shutdown the loop before destroying the FakeSysmem and MockAllocator which
  // may still have pending callbacks.
  loop.Shutdown();
}

TEST_F(ControllerWithFakeSysmemTest, SysmemRequirements) {
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());

  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  EXPECT_OK(display_.DisplayControllerImplImportBufferCollection(
      kBanjoBufferCollectionId, token_endpoints->client.TakeChannel()));

  loop_.RunUntilIdle();

  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(display_.DisplayControllerImplSetBufferCollectionConstraints(&kDisplayUsage,
                                                                         kBanjoBufferCollectionId));

  loop_.RunUntilIdle();

  MockAllocator& allocator = sysmem_;
  MockNoCpuBufferCollection* collection = allocator.GetMostRecentBufferCollection();
  ASSERT_TRUE(collection);
  EXPECT_TRUE(collection->set_constraints_called());
}

TEST_F(ControllerWithFakeSysmemTest, SysmemInvalidType) {
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());

  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  EXPECT_OK(display_.DisplayControllerImplImportBufferCollection(
      kBanjoBufferCollectionId, token_endpoints->client.TakeChannel()));

  loop_.RunUntilIdle();

  static constexpr image_buffer_usage_t kInvalidTilingUsage = {
      .tiling_type = 1000000,
  };
  EXPECT_EQ(ZX_ERR_INVALID_ARGS, display_.DisplayControllerImplSetBufferCollectionConstraints(
                                     &kInvalidTilingUsage, kBanjoBufferCollectionId));

  loop_.RunUntilIdle();

  MockAllocator& allocator = sysmem_;
  MockNoCpuBufferCollection* collection = allocator.GetMostRecentBufferCollection();
  ASSERT_TRUE(collection);
  EXPECT_FALSE(collection->set_constraints_called());
}

// Tests that DDK basic DDK lifecycle hooks function as expected.
TEST_F(IntegrationTest, BindAndInit) {
  PerformBlockingWork([&] { ASSERT_OK(Controller::Create(parent())); });

  // There should be two published devices: one "intel_i915" device rooted at |parent()|, and a
  // grandchild "intel-gpu-core" device.
  ASSERT_EQ(1u, parent()->child_count());
  auto dev = parent()->GetLatestChild();
  ASSERT_EQ(2u, dev->child_count());

  // Perform the async initialization and wait for a response.
  dev->InitOp();
  EXPECT_EQ(ZX_OK, dev->WaitUntilInitReplyCalled());

  // Unbind the device and ensure it completes synchronously.
  dev->UnbindOp();
  EXPECT_TRUE(dev->UnbindReplyCalled());

  mock_ddk::ReleaseFlaggedDevices(parent());
  EXPECT_EQ(0u, dev->child_count());
}

// Tests that the device can initialize even if bootloader framebuffer information is not available
// and global GTT allocations start at offset 0.
TEST_F(IntegrationTest, InitFailsIfBootloaderGetInfoFails) {
  SetFramebuffer({.status = ZX_ERR_INVALID_ARGS});

  PerformBlockingWork([&] { ASSERT_OK(Controller::Create(parent())); });
  auto dev = parent()->GetLatestChild();
  Controller* ctx = dev->GetDeviceContext<Controller>();

  uint64_t addr;
  EXPECT_EQ(ZX_OK, ctx->IntelGpuCoreGttAlloc(1, &addr));
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
  SetFramebuffer({
      .format = ZBI_PIXEL_FORMAT_RGB_888,
      .width = kStride,
      .height = kHeight,
      .stride = kStride,
  });
  PerformBlockingWork([&] { ASSERT_OK(Controller::Create(parent())); });

  // There should be two published devices: one "intel_i915" device rooted at |parent()|, and a
  // grandchild "intel-gpu-core" device.
  ASSERT_EQ(1u, parent()->child_count());
  auto dev = parent()->GetLatestChild();
  Controller* ctx = dev->GetDeviceContext<Controller>();

  uint64_t addr;
  EXPECT_EQ(ZX_OK, ctx->IntelGpuCoreGttAlloc(1, &addr));
  EXPECT_EQ(ZX_ROUNDUP(kHeight * kStride * 3, PAGE_SIZE), addr);
}

TEST_F(IntegrationTest, SysmemImport) {
  PerformBlockingWork([&] { ASSERT_OK(Controller::Create(parent())); });

  // There should be two published devices: one "intel_i915" device rooted at `parent()`, and a
  // grandchild "intel-gpu-core" device.
  ASSERT_EQ(1u, parent()->child_count());
  auto dev = parent()->GetLatestChild();
  Controller* ctx = dev->GetDeviceContext<Controller>();

  // Import buffer collection.
  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());
  EXPECT_OK(ctx->DisplayControllerImplImportBufferCollection(
      kBanjoBufferCollectionId, token_endpoints->client.TakeChannel()));

  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(ctx->DisplayControllerImplSetBufferCollectionConstraints(&kDisplayUsage,
                                                                     kBanjoBufferCollectionId));

  RunLoopUntilIdle();

  MockAllocator& allocator = *sysmem();
  MockNoCpuBufferCollection* collection = allocator.GetMostRecentBufferCollection();
  ASSERT_TRUE(collection);
  EXPECT_TRUE(collection->set_constraints_called());

  static constexpr image_metadata_t kDisplayImageMetadata = {
      .width = 128,
      .height = kImageHeight,
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  uint64_t image_handle = 0;
  PerformBlockingWork([&] {
    EXPECT_OK(ctx->DisplayControllerImplImportImage(
        &kDisplayImageMetadata, kBanjoBufferCollectionId, /*index=*/0, &image_handle));
  });

  const GttRegion& region =
      ctx->SetupGttImage(kDisplayImageMetadata, image_handle, FRAME_TRANSFORM_IDENTITY);
  EXPECT_LT(kDisplayImageMetadata.width * 4, kBytesPerRowDivisor);
  EXPECT_EQ(kBytesPerRowDivisor, region.bytes_per_row());
  ctx->DisplayControllerImplReleaseImage(image_handle);
}

TEST_F(IntegrationTest, SysmemRotated) {
  PerformBlockingWork([&] { ASSERT_OK(Controller::Create(parent())); });

  // There should be two published devices: one "intel_i915" device rooted at `parent()`, and a
  // grandchild "intel-gpu-core" device.
  ASSERT_EQ(1u, parent()->child_count());
  auto dev = parent()->GetLatestChild();
  Controller* ctx = dev->GetDeviceContext<Controller>();

  // Import buffer collection.
  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());
  EXPECT_OK(ctx->DisplayControllerImplImportBufferCollection(
      kBanjoBufferCollectionId, token_endpoints->client.TakeChannel()));

  RunLoopUntilIdle();

  MockAllocator& allocator = *sysmem();
  MockNoCpuBufferCollection* collection = allocator.GetMostRecentBufferCollection();
  ASSERT_TRUE(collection);
  collection->set_format_modifier(fuchsia_images2::wire::PixelFormatModifier::kIntelI915YTiled);

  static constexpr image_buffer_usage_t kTiledDisplayUsage = {
      // Must be y or yf tiled so rotation is allowed.
      .tiling_type = IMAGE_TILING_TYPE_Y_LEGACY_TILED,
  };
  EXPECT_OK(ctx->DisplayControllerImplSetBufferCollectionConstraints(&kTiledDisplayUsage,
                                                                     kBanjoBufferCollectionId));

  RunLoopUntilIdle();
  EXPECT_TRUE(collection->set_constraints_called());

  static constexpr image_metadata_t kTiledImageMetadata = {
      .width = 128,
      .height = kImageHeight,
      .tiling_type = IMAGE_TILING_TYPE_Y_LEGACY_TILED,
  };
  uint64_t image_handle = 0;
  PerformBlockingWork([&]() mutable {
    EXPECT_OK(ctx->DisplayControllerImplImportImage(&kTiledImageMetadata, kBanjoBufferCollectionId,
                                                    /*index=*/0, &image_handle));
  });

  // Check that rotating the image doesn't hang.
  const GttRegion& region =
      ctx->SetupGttImage(kTiledImageMetadata, image_handle, FRAME_TRANSFORM_ROT_90);
  EXPECT_LT(kTiledImageMetadata.width * 4, kBytesPerRowDivisor);
  EXPECT_EQ(kBytesPerRowDivisor, region.bytes_per_row());
  ctx->DisplayControllerImplReleaseImage(image_handle);
}

}  // namespace

}  // namespace i915
