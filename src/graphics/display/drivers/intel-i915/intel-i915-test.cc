// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-i915/intel-i915.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire_test_base.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire_test_base.h>
#include <fuchsia/hardware/sysmem/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/async-loop/testing/cpp/real_loop.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/driver.h>
#include <lib/fidl-async/cpp/bind.h>
#include <lib/mmio-ptr/fake.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zircon-internal/align.h>
#include <lib/zx/vmar.h>

#include <type_traits>
#include <vector>

#include <gtest/gtest.h>

#include "src/devices/pci/testing/pci_protocol_fake.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/graphics/display/drivers/intel-i915/pci-ids.h"
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

// TODO(fxbug.dev/121924): Consider creating and using a unified set of sysmem
// testing doubles instead of writing mocks for each display driver test.
class MockNoCpuBufferCollection
    : public fidl::testing::WireTestBase<fuchsia_sysmem::BufferCollection> {
 public:
  void set_format_modifier(uint64_t format_modifier) { format_modifier_ = format_modifier; }

  bool set_constraints_called() const { return set_constraints_called_; }
  void SetConstraints(SetConstraintsRequestView request,
                      SetConstraintsCompleter::Sync& _completer) override {
    set_constraints_called_ = true;
    if (!request->has_constraints) {
      return;
    }

    EXPECT_FALSE(request->constraints.buffer_memory_constraints.inaccessible_domain_supported);
    EXPECT_FALSE(request->constraints.buffer_memory_constraints.cpu_domain_supported);
    constraints_ = request->constraints;
  }

  void CheckBuffersAllocated(CheckBuffersAllocatedCompleter::Sync& completer) override {
    completer.Reply(ZX_OK);
  }

  void WaitForBuffersAllocated(WaitForBuffersAllocatedCompleter::Sync& completer) override {
    fuchsia_sysmem::wire::BufferCollectionInfo2 info;
    info.settings.has_image_format_constraints = true;
    auto& constraints = info.settings.image_format_constraints;
    for (size_t i = 0; i < constraints_.image_format_constraints_count; i++) {
      if (constraints_.image_format_constraints[i].pixel_format.format_modifier.value ==
          format_modifier_) {
        constraints = constraints_.image_format_constraints[i];
        break;
      }
    }
    constraints.bytes_per_row_divisor = kBytesPerRowDivisor;
    info.buffer_count = 1;
    EXPECT_OK(zx::vmo::create(kBytesPerRowDivisor * kImageHeight, 0, &info.buffers[0].vmo));
    completer.Reply(ZX_OK, std::move(info));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    EXPECT_TRUE(false);
  }

 private:
  bool set_constraints_called_ = false;
  uint64_t format_modifier_ = fuchsia_sysmem::wire::kFormatModifierLinear;
  fuchsia_sysmem::wire::BufferCollectionConstraints constraints_;
};

class MockAllocator : public fidl::testing::WireTestBase<fuchsia_sysmem::Allocator> {
 public:
  explicit MockAllocator(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {
    EXPECT_TRUE(dispatcher_);
  }

  void BindSharedCollection(BindSharedCollectionRequestView request,
                            BindSharedCollectionCompleter::Sync& completer) override {
    const std::vector<fuchsia_sysmem::wire::PixelFormatType> kPixelFormatTypes = {
        fuchsia_sysmem::wire::PixelFormatType::kBgra32,
        fuchsia_sysmem::wire::PixelFormatType::kR8G8B8A8};

    auto buffer_collection_id = next_buffer_collection_id_++;
    active_buffer_collections_[buffer_collection_id] = {
        .token_client = std::move(request->token),
        .mock_buffer_collection = std::make_unique<MockNoCpuBufferCollection>(),
    };
    most_recent_buffer_collection_ =
        active_buffer_collections_.at(buffer_collection_id).mock_buffer_collection.get();

    fidl::BindServer(
        dispatcher_, std::move(request->buffer_collection_request),
        active_buffer_collections_[buffer_collection_id].mock_buffer_collection.get(),
        [this, buffer_collection_id](MockNoCpuBufferCollection*, fidl::UnbindInfo,
                                     fidl::ServerEnd<fuchsia_sysmem::BufferCollection>) {
          inactive_buffer_collection_tokens_.push_back(
              std::move(active_buffer_collections_[buffer_collection_id].token_client));
          active_buffer_collections_.erase(buffer_collection_id);
        });
  }

  void SetDebugClientInfo(SetDebugClientInfoRequestView request,
                          SetDebugClientInfoCompleter::Sync& completer) override {
    EXPECT_EQ(request->name.get().find("intel-i915"), 0u);
  }

  // Returns the most recent created BufferCollection server.
  // This may go out of scope if the caller releases the BufferCollection.
  MockNoCpuBufferCollection* GetMostRecentBufferCollection() const {
    return most_recent_buffer_collection_;
  }

  std::vector<fidl::UnownedClientEnd<fuchsia_sysmem::BufferCollectionToken>>
  GetActiveBufferCollectionTokenClients() const {
    std::vector<fidl::UnownedClientEnd<fuchsia_sysmem::BufferCollectionToken>>
        unowned_token_clients;
    unowned_token_clients.reserve(active_buffer_collections_.size());

    for (const auto& kv : active_buffer_collections_) {
      unowned_token_clients.push_back(kv.second.token_client);
    }
    return unowned_token_clients;
  }

  std::vector<fidl::UnownedClientEnd<fuchsia_sysmem::BufferCollectionToken>>
  GetInactiveBufferCollectionTokenClients() const {
    std::vector<fidl::UnownedClientEnd<fuchsia_sysmem::BufferCollectionToken>>
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
    fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken> token_client;
    std::unique_ptr<MockNoCpuBufferCollection> mock_buffer_collection;
  };

  using BufferCollectionId = int;

  MockNoCpuBufferCollection* most_recent_buffer_collection_ = nullptr;
  std::unordered_map<BufferCollectionId, BufferCollection> active_buffer_collections_;
  std::vector<fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>>
      inactive_buffer_collection_tokens_;

  BufferCollectionId next_buffer_collection_id_ = 0;

  async_dispatcher_t* dispatcher_ = nullptr;
};

class FakeSysmem : public fidl::testing::WireTestBase<fuchsia_hardware_sysmem::Sysmem> {
 public:
  explicit FakeSysmem(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {
    EXPECT_TRUE(dispatcher_);
  }

  void ConnectServer(ConnectServerRequestView request,
                     ConnectServerCompleter::Sync& completer) override {
    mock_allocators_.emplace_front(dispatcher_);
    auto it = mock_allocators_.begin();
    fidl::BindServer(dispatcher_, std::move(request->allocator_request), &*it);
  }

  std::list<MockAllocator>& mock_allocators() { return mock_allocators_; }

  // FIDL methods
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) final {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

 private:
  std::list<MockAllocator> mock_allocators_;
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

    zx::result service_result = outgoing_.AddService<fuchsia_hardware_sysmem::Service>(
        fuchsia_hardware_sysmem::Service::InstanceHandler(
            {.sysmem = sysmem_.bind_handler(dispatcher())}));
    ASSERT_EQ(service_result.status_value(), ZX_OK);

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_EQ(endpoints.status_value(), ZX_OK);
    ASSERT_EQ(outgoing_.Serve(std::move(endpoints->server)).status_value(), ZX_OK);

    parent_->AddFidlService(fuchsia_hardware_sysmem::Service::Name, std::move(endpoints->client),
                            "sysmem-fidl");

    service_result = outgoing_.AddService<fuchsia_hardware_pci::Service>(
        fuchsia_hardware_pci::Service::InstanceHandler(
            {.device = pci_.bind_handler(pci_loop_.dispatcher())}));
    ZX_ASSERT(service_result.is_ok());

    endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(endpoints.is_ok());
    ZX_ASSERT(outgoing_.Serve(std::move(endpoints->server)).is_ok());

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

  FakeSysmem* sysmem() { return &sysmem_; }

 private:
  async::Loop pci_loop_;
  // Emulated parent protocols.
  pci::FakePciProtocol pci_;
  FakeSysmem sysmem_;
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
    auto sysmem_endpoints = fidl::CreateEndpoints<fuchsia_hardware_sysmem::Sysmem>();
    ASSERT_TRUE(sysmem_endpoints.is_ok());
    auto& [sysmem_client, sysmem_server] = sysmem_endpoints.value();
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

  FakeSysmem sysmem_;
  Controller display_;
};

using ControllerWithFakeSysmemTest = FakeSysmemSingleThreadedTest;

TEST_F(ControllerWithFakeSysmemTest, ImportBufferCollection) {
  EXPECT_EQ(sysmem_.mock_allocators().size(), 1u);
  const MockAllocator& allocator = sysmem_.mock_allocators().front();

  zx::result token1_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token1_endpoints.is_ok());
  zx::result token2_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token2_endpoints.is_ok());

  // Test ImportBufferCollection().
  constexpr uint64_t kValidBufferCollectionId = 1u;
  EXPECT_OK(display_.DisplayControllerImplImportBufferCollection(
      kValidBufferCollectionId, token1_endpoints->client.TakeChannel()));

  // `collection_id` must be unused.
  EXPECT_EQ(display_.DisplayControllerImplImportBufferCollection(
                kValidBufferCollectionId, token2_endpoints->client.TakeChannel()),
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
  constexpr uint64_t kInvalidBufferCollectionId = 2u;
  EXPECT_EQ(display_.DisplayControllerImplReleaseBufferCollection(kInvalidBufferCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display_.DisplayControllerImplReleaseBufferCollection(kValidBufferCollectionId));

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
  FakeSysmem fake_sysmem(loop.dispatcher());
  auto sysmem_endpoints = fidl::CreateEndpoints<fuchsia_hardware_sysmem::Sysmem>();
  ASSERT_TRUE(sysmem_endpoints.is_ok());
  auto& [sysmem_client, sysmem_server] = sysmem_endpoints.value();
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
  constexpr uint64_t kBufferCollectionId = 1u;
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());
  EXPECT_OK(display.DisplayControllerImplImportBufferCollection(
      kBufferCollectionId, token_endpoints->client.TakeChannel()));

  const image_t kDefaultImage = {
      .width = 32,
      .height = 32,
      .type = IMAGE_TYPE_SIMPLE,
      .handle = 0u,
  };
  EXPECT_OK(display.DisplayControllerImplSetBufferCollectionConstraints(&kDefaultImage,
                                                                        kBufferCollectionId));

  // Invalid import: bad collection id
  image_t invalid_image = kDefaultImage;
  uint64_t kInvalidCollectionId = 100;
  EXPECT_EQ(display.DisplayControllerImplImportImage(&invalid_image, kInvalidCollectionId, 0),
            ZX_ERR_NOT_FOUND);

  // Invalid import: bad index
  invalid_image = kDefaultImage;
  uint32_t kInvalidIndex = 100;
  EXPECT_EQ(
      display.DisplayControllerImplImportImage(&invalid_image, kBufferCollectionId, kInvalidIndex),
      ZX_ERR_OUT_OF_RANGE);

  // Invalid import: bad type
  invalid_image = kDefaultImage;
  invalid_image.type = IMAGE_TYPE_CAPTURE;
  EXPECT_EQ(
      display.DisplayControllerImplImportImage(&invalid_image, kBufferCollectionId, /*index=*/0),
      ZX_ERR_INVALID_ARGS);

  // Valid import
  image_t valid_image = kDefaultImage;
  EXPECT_EQ(valid_image.handle, 0u);
  EXPECT_OK(display.DisplayControllerImplImportImage(&valid_image, kBufferCollectionId, 0));
  EXPECT_NE(valid_image.handle, 0u);

  display.DisplayControllerImplReleaseImage(&valid_image);

  // Release buffer collection.
  EXPECT_OK(display.DisplayControllerImplReleaseBufferCollection(kBufferCollectionId));

  // Shutdown the loop before destroying the FakeSysmem and MockAllocator which
  // may still have pending callbacks.
  loop.Shutdown();
}

TEST_F(ControllerWithFakeSysmemTest, SysmemRequirements) {
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());

  constexpr uint64_t kBufferCollectionId = 1u;
  EXPECT_OK(display_.DisplayControllerImplImportBufferCollection(
      kBufferCollectionId, token_endpoints->client.TakeChannel()));

  loop_.RunUntilIdle();

  image_t image = {};

  EXPECT_OK(
      display_.DisplayControllerImplSetBufferCollectionConstraints(&image, kBufferCollectionId));

  loop_.RunUntilIdle();

  MockAllocator& allocator = sysmem_.mock_allocators().front();
  MockNoCpuBufferCollection* collection = allocator.GetMostRecentBufferCollection();
  ASSERT_TRUE(collection);
  EXPECT_TRUE(collection->set_constraints_called());
}

TEST_F(ControllerWithFakeSysmemTest, SysmemInvalidType) {
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());

  constexpr uint64_t kBufferCollectionId = 1u;
  EXPECT_OK(display_.DisplayControllerImplImportBufferCollection(
      kBufferCollectionId, token_endpoints->client.TakeChannel()));

  loop_.RunUntilIdle();

  image_t image = {};
  image.type = 1000000;

  EXPECT_EQ(ZX_ERR_INVALID_ARGS, display_.DisplayControllerImplSetBufferCollectionConstraints(
                                     &image, kBufferCollectionId));

  loop_.RunUntilIdle();

  MockAllocator& allocator = sysmem_.mock_allocators().front();
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

// TODO(fxbug.dev/85836): Add tests for DisplayPort display enumeration by InitOp, covering the
// following cases:
//   - Display found during start up but not already powered.
//   - Display found during start up but already powered up.
//   - Display added and removed in a hotplug event.
// TODO(fxbug.dev/86314): Add test for HDMI display enumeration by InitOp.
// TODO(fxbug.dev/86315): Add test for DVI display enumeration by InitOp.

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
  constexpr uint64_t kBufferCollectionId = 1u;
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());
  EXPECT_OK(ctx->DisplayControllerImplImportBufferCollection(
      kBufferCollectionId, token_endpoints->client.TakeChannel()));

  image_t image = {};
  image.width = 128;
  image.height = kImageHeight;
  EXPECT_OK(ctx->DisplayControllerImplSetBufferCollectionConstraints(&image, kBufferCollectionId));

  RunLoopUntilIdle();

  MockAllocator& allocator = sysmem()->mock_allocators().front();
  MockNoCpuBufferCollection* collection = allocator.GetMostRecentBufferCollection();
  ASSERT_TRUE(collection);
  EXPECT_TRUE(collection->set_constraints_called());

  PerformBlockingWork([&] {
    EXPECT_OK(ctx->DisplayControllerImplImportImage(&image, kBufferCollectionId, /*index=*/0));
  });

  const GttRegion& region = ctx->SetupGttImage(&image, FRAME_TRANSFORM_IDENTITY);
  EXPECT_LT(image.width * 4, kBytesPerRowDivisor);
  EXPECT_EQ(kBytesPerRowDivisor, region.bytes_per_row());
  ctx->DisplayControllerImplReleaseImage(&image);
}

TEST_F(IntegrationTest, SysmemRotated) {
  PerformBlockingWork([&] { ASSERT_OK(Controller::Create(parent())); });

  // There should be two published devices: one "intel_i915" device rooted at `parent()`, and a
  // grandchild "intel-gpu-core" device.
  ASSERT_EQ(1u, parent()->child_count());
  auto dev = parent()->GetLatestChild();
  Controller* ctx = dev->GetDeviceContext<Controller>();

  // Import buffer collection.
  constexpr uint64_t kBufferCollectionId = 1u;
  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());
  EXPECT_OK(ctx->DisplayControllerImplImportBufferCollection(
      kBufferCollectionId, token_endpoints->client.TakeChannel()));

  RunLoopUntilIdle();

  MockAllocator& allocator = sysmem()->mock_allocators().front();
  MockNoCpuBufferCollection* collection = allocator.GetMostRecentBufferCollection();
  ASSERT_TRUE(collection);
  collection->set_format_modifier(fuchsia_sysmem::wire::kFormatModifierIntelI915YTiled);

  image_t image = {};
  image.width = 128;
  image.height = kImageHeight;
  // Must match set_format_modifier above, and also be y or yf tiled so rotation is allowed.
  image.type = IMAGE_TYPE_Y_LEGACY_TILED;

  EXPECT_OK(ctx->DisplayControllerImplSetBufferCollectionConstraints(&image, kBufferCollectionId));

  RunLoopUntilIdle();
  EXPECT_TRUE(collection->set_constraints_called());

  image.type = IMAGE_TYPE_Y_LEGACY_TILED;
  PerformBlockingWork([&]() mutable {
    EXPECT_OK(ctx->DisplayControllerImplImportImage(&image, kBufferCollectionId, /*index=*/0));
  });

  // Check that rotating the image doesn't hang.
  const GttRegion& region = ctx->SetupGttImage(&image, FRAME_TRANSFORM_ROT_90);
  EXPECT_LT(image.width * 4, kBytesPerRowDivisor);
  EXPECT_EQ(kBytesPerRowDivisor, region.bytes_per_row());
  ctx->DisplayControllerImplReleaseImage(&image);
}

}  // namespace

}  // namespace i915
