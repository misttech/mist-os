// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/amlogic-display.h"

#include <fidl/fuchsia.sysmem/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl-async/cpp/bind.h>
#include <lib/inspect/cpp/inspect.h>
#include <zircon/syscalls/object.h>

#include "src/graphics/display/drivers/amlogic-display/osd.h"
#include "src/lib/fsl/handles/object_info.h"
#include "zxtest/zxtest.h"

namespace sysmem = fuchsia_sysmem;

// TODO(fxbug.dev/121924): Consider creating and using a unified set of sysmem
// testing doubles instead of writing mocks for each display driver test.
class MockBufferCollection : public fidl::testing::WireTestBase<fuchsia_sysmem::BufferCollection> {
 public:
  MockBufferCollection(const std::vector<sysmem::wire::PixelFormatType>& pixel_format_types =
                           {sysmem::wire::PixelFormatType::kBgra32,
                            sysmem::wire::PixelFormatType::kR8G8B8A8})
      : supported_pixel_format_types_(pixel_format_types) {}

  void SetConstraints(SetConstraintsRequestView request,
                      SetConstraintsCompleter::Sync& _completer) override {
    EXPECT_TRUE(request->constraints.buffer_memory_constraints.inaccessible_domain_supported);
    EXPECT_FALSE(request->constraints.buffer_memory_constraints.cpu_domain_supported);
    EXPECT_EQ(64u, request->constraints.image_format_constraints[0].bytes_per_row_divisor);

    bool has_rgba =
        std::find(supported_pixel_format_types_.begin(), supported_pixel_format_types_.end(),
                  fuchsia_sysmem::wire::PixelFormatType::kR8G8B8A8) !=
        supported_pixel_format_types_.end();
    bool has_bgra =
        std::find(supported_pixel_format_types_.begin(), supported_pixel_format_types_.end(),
                  fuchsia_sysmem::wire::PixelFormatType::kBgra32) !=
        supported_pixel_format_types_.end();

    size_t expected_format_constraints_count = 0u;
    const auto& image_format_constraints = request->constraints.image_format_constraints;
    if (has_bgra) {
      expected_format_constraints_count += 2;
      EXPECT_TRUE(std::find_if(image_format_constraints.begin(),
                               image_format_constraints.begin() +
                                   request->constraints.image_format_constraints_count,
                               [](const auto& format) {
                                 return format.pixel_format.format_modifier.value ==
                                        sysmem::wire::kFormatModifierArmLinearTe;
                               }) != image_format_constraints.end());
    }
    if (has_rgba) {
      expected_format_constraints_count += 2;
    }

    EXPECT_EQ(expected_format_constraints_count,
              request->constraints.image_format_constraints_count);
    set_constraints_called_ = true;
  }

  void SetName(SetNameRequestView request, SetNameCompleter::Sync& completer) override {
    EXPECT_EQ(10u, request->priority);
    EXPECT_EQ(std::string("Display"), std::string(request->name.data(), request->name.size()));
    set_name_called_ = true;
  }

  void CheckBuffersAllocated(CheckBuffersAllocatedCompleter::Sync& completer) override {
    completer.Reply(ZX_OK);
  }

  void WaitForBuffersAllocated(WaitForBuffersAllocatedCompleter::Sync& completer) override {
    sysmem::wire::BufferCollectionInfo2 collection;
    collection.buffer_count = 1;
    collection.settings.has_image_format_constraints = true;
    auto& image_constraints = collection.settings.image_format_constraints;
    image_constraints.min_bytes_per_row = 4;
    image_constraints.min_coded_height = 4;
    image_constraints.pixel_format.type = sysmem::wire::PixelFormatType::kBgr24;
    EXPECT_EQ(ZX_OK, zx::vmo::create(ZX_PAGE_SIZE, 0u, &collection.buffers[0].vmo));
    completer.Reply(ZX_OK, std::move(collection));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    EXPECT_TRUE(false);
  }

  bool set_constraints_called() const { return set_constraints_called_; }
  bool set_name_called() const { return set_name_called_; }

 private:
  bool set_constraints_called_ = false;
  bool set_name_called_ = false;
  std::vector<sysmem::wire::PixelFormatType> supported_pixel_format_types_;
};

class MockAllocator : public fidl::testing::WireTestBase<fuchsia_sysmem::Allocator> {
 public:
  explicit MockAllocator(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {
    ASSERT_TRUE(dispatcher_);
  }

  void BindSharedCollection(BindSharedCollectionRequestView request,
                            BindSharedCollectionCompleter::Sync& completer) override {
    const std::vector<sysmem::wire::PixelFormatType> kPixelFormatTypes = {
        sysmem::wire::PixelFormatType::kBgra32, sysmem::wire::PixelFormatType::kR8G8B8A8};

    auto buffer_collection_id = next_buffer_collection_id_++;
    active_buffer_collections_[buffer_collection_id] = {
        .token_client = std::move(request->token),
        .mock_buffer_collection = std::make_unique<MockBufferCollection>(kPixelFormatTypes),
    };

    fidl::BindServer(
        dispatcher_, std::move(request->buffer_collection_request),
        active_buffer_collections_[buffer_collection_id].mock_buffer_collection.get(),
        [this, buffer_collection_id](MockBufferCollection*, fidl::UnbindInfo,
                                     fidl::ServerEnd<fuchsia_sysmem::BufferCollection>) {
          inactive_buffer_collection_tokens_.push_back(
              std::move(active_buffer_collections_[buffer_collection_id].token_client));
          active_buffer_collections_.erase(buffer_collection_id);
        });
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
    std::unique_ptr<MockBufferCollection> mock_buffer_collection;
  };

  using BufferCollectionId = int;

  std::unordered_map<BufferCollectionId, BufferCollection> active_buffer_collections_;
  std::vector<fidl::ClientEnd<fuchsia_sysmem::BufferCollectionToken>>
      inactive_buffer_collection_tokens_;

  BufferCollectionId next_buffer_collection_id_ = 0;

  async_dispatcher_t* dispatcher_ = nullptr;
};

class FakeCanvasProtocol : ddk::AmlogicCanvasProtocol<FakeCanvasProtocol> {
 public:
  zx_status_t AmlogicCanvasConfig(zx::vmo vmo, size_t offset, const canvas_info_t* info,
                                  uint8_t* canvas_idx) {
    for (size_t i = 0; i < std::size(in_use_); i++) {
      ZX_DEBUG_ASSERT_MSG(i <= std::numeric_limits<uint8_t>::max(), "%zu", i);
      if (!in_use_[i]) {
        in_use_[i] = true;
        *canvas_idx = static_cast<uint8_t>(i);
        return ZX_OK;
      }
    }
    return ZX_ERR_NO_MEMORY;
  }
  zx_status_t AmlogicCanvasFree(uint8_t canvas_idx) {
    EXPECT_TRUE(in_use_[canvas_idx]);
    in_use_[canvas_idx] = false;
    return ZX_OK;
  }

  void CheckThatNoEntriesInUse() {
    for (uint32_t i = 0; i < std::size(in_use_); i++) {
      EXPECT_FALSE(in_use_[i]);
    }
  }

  const amlogic_canvas_protocol_t& get_protocol() { return protocol_; }

 private:
  static constexpr uint32_t kCanvasEntries = 256;
  bool in_use_[kCanvasEntries] = {};
  amlogic_canvas_protocol_t protocol_ = {.ops = &amlogic_canvas_protocol_ops_, .ctx = this};
};

TEST(AmlogicDisplay, ImportBufferCollection) {
  amlogic_display::AmlogicDisplay display(nullptr);
  display.SetFormatSupportCheck([](auto) { return true; });

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  zx::result allocator_endpoints = fidl::CreateEndpoints<sysmem::Allocator>();
  ASSERT_OK(allocator_endpoints);
  MockAllocator allocator(loop.dispatcher());
  ASSERT_OK(fidl::BindSingleInFlightOnly(loop.dispatcher(), std::move(allocator_endpoints->server),
                                         &allocator));
  display.SetSysmemAllocatorForTesting(
      fidl::WireSyncClient(std::move(allocator_endpoints->client)));

  zx::result token1_endpoints = fidl::CreateEndpoints<sysmem::BufferCollectionToken>();
  ASSERT_OK(token1_endpoints);
  zx::result token2_endpoints = fidl::CreateEndpoints<sysmem::BufferCollectionToken>();
  ASSERT_OK(token2_endpoints);

  // Test ImportBufferCollection().
  constexpr uint64_t kValidBufferCollectionId = 1u;
  EXPECT_OK(display.DisplayControllerImplImportBufferCollection(
      kValidBufferCollectionId, token1_endpoints->client.TakeChannel()));

  // `collection_id` must be unused.
  EXPECT_EQ(display.DisplayControllerImplImportBufferCollection(
                kValidBufferCollectionId, token2_endpoints->client.TakeChannel()),
            ZX_ERR_ALREADY_EXISTS);

  loop.RunUntilIdle();

  // Verify that the current buffer collection token is used (active).
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
  EXPECT_EQ(display.DisplayControllerImplReleaseBufferCollection(kInvalidBufferCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display.DisplayControllerImplReleaseBufferCollection(kValidBufferCollectionId));

  loop.RunUntilIdle();

  // Verify that the current buffer collection token is released (inactive).
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

  // Shutdown the loop before destroying the MockAllocator which may still have
  // pending callbacks.
  loop.Shutdown();
}

TEST(AmlogicDisplay, SysmemRequirements) {
  amlogic_display::AmlogicDisplay display(nullptr);
  display.SetFormatSupportCheck([](auto) { return true; });
  zx::result buffer_collection_endpoints = fidl::CreateEndpoints<sysmem::BufferCollection>();
  ASSERT_OK(buffer_collection_endpoints);

  MockBufferCollection collection(
      {sysmem::wire::PixelFormatType::kBgra32, sysmem::wire::PixelFormatType::kR8G8B8A8});
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  image_t image = {};
  ASSERT_OK(fidl::BindSingleInFlightOnly(
      loop.dispatcher(), std::move(buffer_collection_endpoints->server), &collection));

  EXPECT_OK(display.DisplayControllerImplSetBufferCollectionConstraints(
      &image, buffer_collection_endpoints->client.handle()->get()));

  loop.RunUntilIdle();
  EXPECT_TRUE(collection.set_constraints_called());
  EXPECT_TRUE(collection.set_name_called());
}

TEST(AmlogicDisplay, SysmemRequirements_BgraOnly) {
  amlogic_display::AmlogicDisplay display(nullptr);
  display.SetFormatSupportCheck([](zx_pixel_format_t format) {
    return format == ZX_PIXEL_FORMAT_RGB_x888 || format == ZX_PIXEL_FORMAT_ARGB_8888;
  });
  zx::result buffer_collection_endpoints = fidl::CreateEndpoints<sysmem::BufferCollection>();
  ASSERT_OK(buffer_collection_endpoints);

  MockBufferCollection collection({sysmem::wire::PixelFormatType::kBgra32});
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  image_t image = {};
  ASSERT_OK(fidl::BindSingleInFlightOnly(
      loop.dispatcher(), std::move(buffer_collection_endpoints->server), &collection));

  EXPECT_OK(display.DisplayControllerImplSetBufferCollectionConstraints(
      &image, buffer_collection_endpoints->client.handle()->get()));

  loop.RunUntilIdle();
  EXPECT_TRUE(collection.set_constraints_called());
  EXPECT_TRUE(collection.set_name_called());
}

TEST(AmlogicDisplay, FloatToFix3_10) {
  inspect::Inspector inspector;
  EXPECT_EQ(0x0000, amlogic_display::Osd::FloatToFixed3_10(0.0f));
  EXPECT_EQ(0x0066, amlogic_display::Osd::FloatToFixed3_10(0.1f));
  EXPECT_EQ(0x1f9a, amlogic_display::Osd::FloatToFixed3_10(-0.1f));
  // Test for maximum positive (<4)
  EXPECT_EQ(0x0FFF, amlogic_display::Osd::FloatToFixed3_10(4.0f));
  EXPECT_EQ(0x0FFF, amlogic_display::Osd::FloatToFixed3_10(40.0f));
  EXPECT_EQ(0x0FFF, amlogic_display::Osd::FloatToFixed3_10(3.9999f));
  // Test for minimum negative (>= -4)
  EXPECT_EQ(0x1000, amlogic_display::Osd::FloatToFixed3_10(-4.0f));
  EXPECT_EQ(0x1000, amlogic_display::Osd::FloatToFixed3_10(-14.0f));
}

TEST(AmlogicDisplay, FloatToFixed2_10) {
  inspect::Inspector inspector;
  EXPECT_EQ(0x0000, amlogic_display::Osd::FloatToFixed2_10(0.0f));
  EXPECT_EQ(0x0066, amlogic_display::Osd::FloatToFixed2_10(0.1f));
  EXPECT_EQ(0x0f9a, amlogic_display::Osd::FloatToFixed2_10(-0.1f));
  // Test for maximum positive (<2)
  EXPECT_EQ(0x07FF, amlogic_display::Osd::FloatToFixed2_10(2.0f));
  EXPECT_EQ(0x07FF, amlogic_display::Osd::FloatToFixed2_10(20.0f));
  EXPECT_EQ(0x07FF, amlogic_display::Osd::FloatToFixed2_10(1.9999f));
  // Test for minimum negative (>= -2)
  EXPECT_EQ(0x0800, amlogic_display::Osd::FloatToFixed2_10(-2.0f));
  EXPECT_EQ(0x0800, amlogic_display::Osd::FloatToFixed2_10(-14.0f));
}

TEST(AmlogicDisplay, NoLeakCaptureCanvas) {
  amlogic_display::AmlogicDisplay display(nullptr);
  display.SetFormatSupportCheck([](auto) { return true; });
  zx::result buffer_collection_endpoints = fidl::CreateEndpoints<sysmem::BufferCollection>();
  ASSERT_OK(buffer_collection_endpoints);

  MockBufferCollection collection(
      {sysmem::wire::PixelFormatType::kBgra32, sysmem::wire::PixelFormatType::kR8G8B8A8});
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  ASSERT_OK(fidl::BindSingleInFlightOnly(
      loop.dispatcher(), std::move(buffer_collection_endpoints->server), &collection));
  loop.StartThread("sysmem-thread");
  FakeCanvasProtocol canvas;
  display.SetCanvasForTesting(ddk::AmlogicCanvasProtocolClient(&canvas.get_protocol()));

  uint64_t capture_handle;
  EXPECT_OK(display.DisplayControllerImplImportImageForCapture(
      buffer_collection_endpoints->client.handle()->get(), 0, &capture_handle));
  EXPECT_OK(display.DisplayControllerImplReleaseCapture(capture_handle));

  canvas.CheckThatNoEntriesInUse();
}
