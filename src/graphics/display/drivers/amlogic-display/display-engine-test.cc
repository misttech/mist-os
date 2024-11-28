// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/display-engine.h"

#include <fidl/fuchsia.component.runner/cpp/fidl.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire_test_base.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/inspect/cpp/inspect.h>

#include <memory>
#include <utility>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/amlogic-display/pixel-grid-size2d.h"
#include "src/graphics/display/drivers/amlogic-display/structured_config.h"
#include "src/graphics/display/drivers/amlogic-display/video-input-unit.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/driver-utils/poll-until.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/testing/predicates/status.h"

namespace amlogic_display {

namespace {

// TODO(https://fxbug.dev/42072949): Consider creating and using a unified set of sysmem
// testing doubles instead of writing mocks for each display driver test.
class MockBufferCollectionBase
    : public fidl::testing::WireTestBase<fuchsia_sysmem2::BufferCollection> {
 public:
  MockBufferCollectionBase() = default;
  ~MockBufferCollectionBase() override = default;

  virtual void VerifyBufferCollectionConstraints(
      const fuchsia_sysmem2::wire::BufferCollectionConstraints& constraints) = 0;
  virtual void VerifyName(const std::string& name) = 0;

  void SetConstraints(SetConstraintsRequestView request,
                      SetConstraintsCompleter::Sync& completer) override {
    if (!request->has_constraints()) {
      return;
    }
    VerifyBufferCollectionConstraints(request->constraints());
    set_constraints_called_ = true;
  }

  void SetName(SetNameRequestView request, SetNameCompleter::Sync& completer) override {
    EXPECT_EQ(10u, request->priority());
    VerifyName(std::string(request->name().data(), request->name().size()));
    set_name_called_ = true;
  }

  void CheckAllBuffersAllocated(CheckAllBuffersAllocatedCompleter::Sync& completer) override {
    completer.Reply(fit::ok());
  }

  void WaitForAllBuffersAllocated(WaitForAllBuffersAllocatedCompleter::Sync& completer) override {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(ZX_PAGE_SIZE, 0u, &vmo));
    auto collection = fuchsia_sysmem2::wire::BufferCollectionInfo::Builder(arena_)
                          .buffers(std::vector{fuchsia_sysmem2::wire::VmoBuffer::Builder(arena_)
                                                   .vmo(std::move(vmo))
                                                   .vmo_usable_start(0)
                                                   .Build()})
                          .settings(fuchsia_sysmem2::wire::SingleBufferSettings::Builder(arena_)
                                        .image_format_constraints(image_format_constraints_)
                                        .Build())
                          .Build();
    auto response =
        fuchsia_sysmem2::wire::BufferCollectionWaitForAllBuffersAllocatedResponse::Builder(arena_)
            .buffer_collection_info(collection)
            .Build();
    completer.Reply(fit::ok(&response));
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    EXPECT_TRUE(false);
  }

  void set_allocated_image_format_constraints(
      const fuchsia_sysmem2::wire::ImageFormatConstraints& image_format_constraints) {
    image_format_constraints_ = image_format_constraints;
  }
  bool set_constraints_called() const { return set_constraints_called_; }
  bool set_name_called() const { return set_name_called_; }

 private:
  fidl::Arena<fidl::kDefaultArenaInitialCapacity> arena_;
  bool set_constraints_called_ = false;
  bool set_name_called_ = false;
  fuchsia_sysmem2::wire::ImageFormatConstraints image_format_constraints_;
};

class MockBufferCollection : public MockBufferCollectionBase {
 public:
  explicit MockBufferCollection(
      const std::vector<fuchsia_images2::wire::PixelFormat>& pixel_format_types =
          {fuchsia_images2::wire::PixelFormat::kB8G8R8A8,
           fuchsia_images2::wire::PixelFormat::kR8G8B8A8})
      : supported_pixel_format_types_(pixel_format_types) {
    set_allocated_image_format_constraints(
        fuchsia_sysmem2::wire::ImageFormatConstraints::Builder(arena_)
            .pixel_format(fuchsia_images2::wire::PixelFormat::kB8G8R8A8)
            .pixel_format_modifier(fuchsia_images2::wire::PixelFormatModifier::kLinear)
            .min_size(fuchsia_math::wire::SizeU{.width = 1, .height = 4})
            .min_bytes_per_row(4096)
            .Build());
  }
  ~MockBufferCollection() override = default;

  void VerifyBufferCollectionConstraints(
      const fuchsia_sysmem2::wire::BufferCollectionConstraints& constraints) override {
    EXPECT_TRUE(constraints.buffer_memory_constraints().inaccessible_domain_supported());
    EXPECT_FALSE(constraints.buffer_memory_constraints().cpu_domain_supported());
    EXPECT_EQ(64u, constraints.image_format_constraints().at(0).bytes_per_row_divisor());

    size_t expected_format_constraints_count = 0u;
    const cpp20::span<const fuchsia_sysmem2::wire::ImageFormatConstraints> image_format_constraints(
        constraints.image_format_constraints().data(),
        constraints.image_format_constraints().count());

    const bool has_bgra =
        std::find(supported_pixel_format_types_.begin(), supported_pixel_format_types_.end(),
                  fuchsia_images2::wire::PixelFormat::kB8G8R8A8) !=
        supported_pixel_format_types_.end();
    if (has_bgra) {
      expected_format_constraints_count += 2;
      const bool image_constraints_contains_bgra32_and_linear = std::any_of(
          image_format_constraints.begin(), image_format_constraints.end(),
          [](const fuchsia_sysmem2::wire::ImageFormatConstraints& format) {
            return format.pixel_format() == fuchsia_images2::wire::PixelFormat::kB8G8R8A8 &&
                   format.pixel_format_modifier() ==
                       fuchsia_images2::wire::PixelFormatModifier::kLinear;
          });
      EXPECT_TRUE(image_constraints_contains_bgra32_and_linear);
    }

    const bool has_rgba =
        std::find(supported_pixel_format_types_.begin(), supported_pixel_format_types_.end(),
                  fuchsia_images2::wire::PixelFormat::kR8G8B8A8) !=
        supported_pixel_format_types_.end();
    if (has_rgba) {
      expected_format_constraints_count += 4;
      const bool image_constraints_contains_rgba32_and_linear = std::any_of(
          image_format_constraints.begin(), image_format_constraints.end(),
          [](const fuchsia_sysmem2::wire::ImageFormatConstraints& format) {
            return format.pixel_format() == fuchsia_images2::wire::PixelFormat::kR8G8B8A8 &&
                   format.pixel_format_modifier() ==
                       fuchsia_images2::wire::PixelFormatModifier::kLinear;
          });
      EXPECT_TRUE(image_constraints_contains_rgba32_and_linear);
      const bool image_constraints_contains_rgba32_and_afbc_16x16 = std::any_of(
          image_format_constraints.begin(), image_format_constraints.end(),
          [](const fuchsia_sysmem2::wire::ImageFormatConstraints& format) {
            return format.pixel_format() == fuchsia_images2::wire::PixelFormat::kR8G8B8A8 &&
                   format.pixel_format_modifier() ==
                       fuchsia_images2::wire::PixelFormatModifier::kArmAfbc16X16SplitBlockSparseYuv;
          });
      EXPECT_TRUE(image_constraints_contains_rgba32_and_afbc_16x16);
    }

    EXPECT_EQ(expected_format_constraints_count, constraints.image_format_constraints().count());
  }

  void VerifyName(const std::string& name) override { EXPECT_EQ(name, "Display"); }

 private:
  fidl::Arena<fidl::kDefaultArenaInitialCapacity> arena_;
  std::vector<fuchsia_images2::wire::PixelFormat> supported_pixel_format_types_;
};

class MockBufferCollectionForCapture : public MockBufferCollectionBase {
 public:
  MockBufferCollectionForCapture() {
    set_allocated_image_format_constraints(
        fuchsia_sysmem2::wire::ImageFormatConstraints::Builder(arena_)
            .pixel_format(fuchsia_images2::wire::PixelFormat::kB8G8R8)
            .pixel_format_modifier(fuchsia_images2::wire::PixelFormatModifier::kLinear)
            .min_size(fuchsia_math::wire::SizeU{.width = 1, .height = 4})
            .min_bytes_per_row(4096)
            .Build());
  }
  ~MockBufferCollectionForCapture() override = default;

  void VerifyBufferCollectionConstraints(
      const fuchsia_sysmem2::wire::BufferCollectionConstraints& constraints) override {
    EXPECT_TRUE(constraints.buffer_memory_constraints().inaccessible_domain_supported());
    EXPECT_FALSE(constraints.buffer_memory_constraints().cpu_domain_supported());
    EXPECT_EQ(64u, constraints.image_format_constraints().at(0).bytes_per_row_divisor());

    size_t expected_format_constraints_count = 1u;
    EXPECT_EQ(expected_format_constraints_count, constraints.image_format_constraints().count());

    const auto& image_format_constraints = constraints.image_format_constraints();
    EXPECT_EQ(image_format_constraints.at(0).pixel_format(),
              fuchsia_images2::wire::PixelFormat::kB8G8R8);
    EXPECT_TRUE(image_format_constraints.at(0).has_pixel_format_modifier());
    EXPECT_EQ(image_format_constraints.at(0).pixel_format_modifier(),
              fuchsia_images2::wire::PixelFormatModifier::kLinear);
  }

  void VerifyName(const std::string& name) override { EXPECT_EQ(name, "Display capture"); }

 private:
  fidl::Arena<fidl::kDefaultArenaInitialCapacity> arena_;
};

class MockAllocator : public fidl::testing::WireTestBase<fuchsia_sysmem2::Allocator> {
 public:
  using MockBufferCollectionBuilder =
      fit::function<std::unique_ptr<MockBufferCollectionBase>(void)>;

  explicit MockAllocator(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {
    ZX_ASSERT(dispatcher_ != nullptr);
  }

  void set_mock_buffer_collection_builder(MockBufferCollectionBuilder builder) {
    mock_buffer_collection_builder_ = std::move(builder);
  }

  void BindSharedCollection(BindSharedCollectionRequestView request,
                            BindSharedCollectionCompleter::Sync& completer) override {
    ZX_ASSERT(mock_buffer_collection_builder_ != nullptr);
    auto buffer_collection_id = next_buffer_collection_id_++;
    active_buffer_collections_[buffer_collection_id] = {
        .token_client = std::move(request->token()),
        .mock_buffer_collection = mock_buffer_collection_builder_(),
    };

    fidl::BindServer(
        dispatcher_, std::move(request->buffer_collection_request()),
        active_buffer_collections_[buffer_collection_id].mock_buffer_collection.get(),
        [this, buffer_collection_id](MockBufferCollectionBase*, fidl::UnbindInfo,
                                     fidl::ServerEnd<fuchsia_sysmem2::BufferCollection>) {
          inactive_buffer_collection_tokens_.push_back(
              std::move(active_buffer_collections_[buffer_collection_id].token_client));
          active_buffer_collections_.erase(buffer_collection_id);
        });
  }

  MockBufferCollectionBase* GetMostRecentBufferCollection() {
    const display::DriverBufferCollectionId most_recent_collection_id(
        next_buffer_collection_id_.value() - 1);
    if (active_buffer_collections_.find(most_recent_collection_id) ==
        active_buffer_collections_.end()) {
      return nullptr;
    }
    return active_buffer_collections_.at(most_recent_collection_id).mock_buffer_collection.get();
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
    std::unique_ptr<MockBufferCollectionBase> mock_buffer_collection;
  };

  std::unordered_map<display::DriverBufferCollectionId, BufferCollection>
      active_buffer_collections_;
  std::vector<fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken>>
      inactive_buffer_collection_tokens_;

  display::DriverBufferCollectionId next_buffer_collection_id_ =
      display::DriverBufferCollectionId(0);
  MockBufferCollectionBuilder mock_buffer_collection_builder_ = nullptr;

  async_dispatcher_t* dispatcher_ = nullptr;
};

class FakeCanvasProtocol : public fidl::WireServer<fuchsia_hardware_amlogiccanvas::Device> {
 public:
  explicit FakeCanvasProtocol(async_dispatcher_t* dispatcher = nullptr)
      : dispatcher_(dispatcher ? dispatcher : async_get_default_dispatcher()) {}

  void Serve(fidl::ServerEnd<fuchsia_hardware_amlogiccanvas::Device> server_end) {
    binding_.emplace(dispatcher_, std::move(server_end), this,
                     std::mem_fn(&FakeCanvasProtocol::OnFidlClosed));
  }

  void OnFidlClosed(fidl::UnbindInfo info) {}

  void Config(ConfigRequestView request, ConfigCompleter::Sync& completer) override {
    for (size_t i = 0; i < std::size(in_use_); i++) {
      ZX_DEBUG_ASSERT_MSG(i <= std::numeric_limits<uint8_t>::max(), "%zu", i);
      if (!in_use_[i]) {
        in_use_[i] = true;
        completer.ReplySuccess(static_cast<uint8_t>(i));
        return;
      }
    }
    completer.ReplyError(ZX_ERR_NO_MEMORY);
  }

  void Free(FreeRequestView request, FreeCompleter::Sync& completer) override {
    EXPECT_TRUE(in_use_[request->canvas_idx]);
    in_use_[request->canvas_idx] = false;
    completer.ReplySuccess();
  }

  void CheckThatNoEntriesInUse() {
    for (uint32_t i = 0; i < std::size(in_use_); i++) {
      EXPECT_FALSE(in_use_[i]);
    }
  }

 private:
  async_dispatcher_t* dispatcher_;
  static constexpr uint32_t kCanvasEntries = 256;
  bool in_use_[kCanvasEntries] = {};
  std::optional<fidl::ServerBinding<fuchsia_hardware_amlogiccanvas::Device>> binding_;
};

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
class FakeSysmemTest : public testing::Test {
 public:
  static constexpr int kWidth = 1024;
  static constexpr int kHeight = 600;

  FakeSysmemTest() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {}

  void InitializeTestEnvironment() {
    auto [directory_client, directory_server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
    std::vector<fuchsia_component_runner::ComponentNamespaceEntry> entries;
    entries.push_back({{.path = "/", .directory = std::move(directory_client)}});
    zx::result<fdf::Namespace> namespace_result = fdf::Namespace::Create(entries);
    ASSERT_OK(namespace_result);

    incoming_ = std::make_shared<fdf::Namespace>(std::move(namespace_result).value());

    zx::result<> test_environment_init_result = test_environment_.SyncCall(
        &fdf_testing::internal::TestEnvironment::Initialize, std::move(directory_server));
    ASSERT_OK(test_environment_init_result);
  }

  void SetUp() override {
    loop_.StartThread("sysmem-handler-loop");
    auto endpoints = fidl::Endpoints<fuchsia_hardware_amlogiccanvas::Device>::Create();
    canvas_.SyncCall(&FakeCanvasProtocol::Serve, std::move(endpoints.server));

    InitializeTestEnvironment();

    display_engine_ = std::make_unique<DisplayEngine>(incoming_, structured_config::Config());
    display_engine_->SetFormatSupportCheck([](auto) { return true; });
    display_engine_->SetCanvasForTesting(std::move(endpoints.client));

    zx::result<std::unique_ptr<Vout>> create_dsi_vout_result = Vout::CreateDsiVoutForTesting(
        /*panel_type=*/PANEL_BOE_TV070WSM_FITIPOWER_JD9364_ASTRO, /*width=*/kWidth,
        /*height=*/kHeight);
    ASSERT_OK(create_dsi_vout_result);
    display_engine_->SetVoutForTesting(std::move(create_dsi_vout_result).value());

    PixelGridSize2D layer_image_size = {
        .width = kWidth,
        .height = kHeight,
    };
    PixelGridSize2D display_contents_size = {
        .width = kWidth,
        .height = kHeight,
    };
    zx::result<std::unique_ptr<VideoInputUnit>> video_input_unit_result =
        VideoInputUnit::CreateForTesting(vpu_mmio_.GetMmioBuffer(), /*rdma=*/nullptr,
                                         layer_image_size, display_contents_size);
    ASSERT_OK(video_input_unit_result);
    display_engine_->SetVideoInputUnitForTesting(std::move(video_input_unit_result).value());

    allocator_ = std::make_unique<MockAllocator>(loop_.dispatcher());
    allocator_->set_mock_buffer_collection_builder([] {
      // Allocate importable primary Image by default.
      const std::vector<fuchsia_images2::wire::PixelFormat> kPixelFormats = {
          fuchsia_images2::wire::PixelFormat::kB8G8R8A8,
          fuchsia_images2::wire::PixelFormat::kR8G8B8A8};
      return std::make_unique<MockBufferCollection>(kPixelFormats);
    });

    {
      auto endpoints = fidl::Endpoints<fuchsia_sysmem2::Allocator>::Create();
      fidl::BindServer(loop_.dispatcher(), std::move(endpoints.server), allocator_.get());
      display_engine_->SetSysmemAllocatorForTesting(
          fidl::WireSyncClient(std::move(endpoints.client)));
    }
  }

  void TearDown() override {
    // Shutdown the loop before destroying the MockAllocator which may still have
    // pending callbacks.
    loop_.Shutdown();
    loop_.JoinThreads();
  }

 protected:
  fdf_testing::ScopedGlobalLogger logger_;

  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  async_patterns::TestDispatcherBound<fdf_testing::internal::TestEnvironment> test_environment_{
      env_dispatcher_->async_dispatcher(), std::in_place};

  std::shared_ptr<fdf::Namespace> incoming_;

  async::Loop loop_;

  ddk_fake::FakeMmioRegRegion vpu_mmio_ =
      ddk_fake::FakeMmioRegRegion(/*reg_size=*/4, /*reg_count=*/0x10000);
  std::unique_ptr<DisplayEngine> display_engine_;
  std::unique_ptr<MockAllocator> allocator_;
  async_patterns::TestDispatcherBound<FakeCanvasProtocol> canvas_{loop_.dispatcher(),
                                                                  std::in_place};
};

TEST_F(FakeSysmemTest, ImportBufferCollection) {
  zx::result<fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>> token1_endpoints =
      fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_OK(token1_endpoints);
  zx::result<fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>> token2_endpoints =
      fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_OK(token2_endpoints);

  // Test ImportBufferCollection().
  constexpr display::DriverBufferCollectionId kValidBufferCollectionId(1);
  constexpr uint64_t kBanjoValidBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kValidBufferCollectionId);
  EXPECT_OK(display_engine_->DisplayEngineImportBufferCollection(
      kBanjoValidBufferCollectionId, token1_endpoints->client.TakeChannel()));

  // `driver_buffer_collection_id` must be unused.
  EXPECT_EQ(display_engine_->DisplayEngineImportBufferCollection(
                kBanjoValidBufferCollectionId, token2_endpoints->client.TakeChannel()),
            ZX_ERR_ALREADY_EXISTS);

  EXPECT_TRUE(display::PollUntil(
      [&]() { return !allocator_->GetActiveBufferCollectionTokenClients().empty(); }, zx::msec(5),
      1000));

  // Verify that the current buffer collection token is used (active).
  {
    auto active_buffer_token_clients = allocator_->GetActiveBufferCollectionTokenClients();
    EXPECT_EQ(active_buffer_token_clients.size(), 1u);

    auto inactive_buffer_token_clients = allocator_->GetInactiveBufferCollectionTokenClients();
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
  EXPECT_EQ(display_engine_->DisplayEngineReleaseBufferCollection(kBanjoInvalidBufferCollectionId),
            ZX_ERR_NOT_FOUND);
  EXPECT_OK(display_engine_->DisplayEngineReleaseBufferCollection(kBanjoValidBufferCollectionId));

  EXPECT_TRUE(display::PollUntil(
      [&]() { return allocator_->GetActiveBufferCollectionTokenClients().empty(); }, zx::msec(5),
      1000));

  // Verify that the current buffer collection token is released (inactive).
  {
    auto active_buffer_token_clients = allocator_->GetActiveBufferCollectionTokenClients();
    EXPECT_EQ(active_buffer_token_clients.size(), 0u);

    auto inactive_buffer_token_clients = allocator_->GetInactiveBufferCollectionTokenClients();
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

TEST_F(FakeSysmemTest, ImportImage) {
  auto [token_client, token_server] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();

  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  EXPECT_OK(display_engine_->DisplayEngineImportBufferCollection(kBanjoBufferCollectionId,
                                                                 token_client.TakeChannel()));

  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(display_engine_->DisplayEngineSetBufferCollectionConstraints(&kDisplayUsage,
                                                                         kBanjoBufferCollectionId));

  constexpr display::DriverBufferCollectionId kInvalidBufferCollectionId(100);
  constexpr uint64_t kBanjoInvalidBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kInvalidBufferCollectionId);
  EXPECT_EQ(display_engine_->DisplayEngineSetBufferCollectionConstraints(
                &kDisplayUsage, kBanjoInvalidBufferCollectionId),
            ZX_ERR_NOT_FOUND);

  // Invalid import: Bad image type.
  static constexpr image_metadata_t kInvalidTilingMetadata = {
      .width = 1024,
      .height = 768,
      .tiling_type = IMAGE_TILING_TYPE_CAPTURE,
  };
  uint64_t image_handle = 0;
  EXPECT_EQ(
      display_engine_->DisplayEngineImportImage(&kInvalidTilingMetadata, kBanjoBufferCollectionId,
                                                /*index=*/0, &image_handle),
      ZX_ERR_INVALID_ARGS);

  // Invalid import: Invalid collection ID.
  static constexpr image_metadata_t kDisplayImageMetadata = {
      .width = 1024,
      .height = 768,
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_EQ(display_engine_->DisplayEngineImportImage(&kDisplayImageMetadata,
                                                      kBanjoInvalidBufferCollectionId,
                                                      /*index=*/0, &image_handle),
            ZX_ERR_NOT_FOUND);

  // Invalid import: Invalid buffer collection index.
  constexpr uint64_t kInvalidBufferCollectionIndex = 100u;
  image_handle = 0;
  EXPECT_EQ(
      display_engine_->DisplayEngineImportImage(&kDisplayImageMetadata, kBanjoBufferCollectionId,
                                                kInvalidBufferCollectionIndex, &image_handle),
      ZX_ERR_OUT_OF_RANGE);

  // Valid import.
  EXPECT_OK(display_engine_->DisplayEngineImportImage(&kDisplayImageMetadata,
                                                      kBanjoBufferCollectionId,
                                                      /*index=*/0, &image_handle));
  EXPECT_NE(image_handle, 0u);

  // Release the image.
  display_engine_->DisplayEngineReleaseImage(image_handle);

  EXPECT_OK(display_engine_->DisplayEngineReleaseBufferCollection(kBanjoBufferCollectionId));
}

TEST_F(FakeSysmemTest, ImportImageForCapture) {
  allocator_->set_mock_buffer_collection_builder(
      [] { return std::make_unique<MockBufferCollectionForCapture>(); });

  auto [token_client, token_server] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();

  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  EXPECT_OK(display_engine_->DisplayEngineImportBufferCollection(kBanjoBufferCollectionId,
                                                                 token_client.TakeChannel()));

  // Driver sets BufferCollection buffer memory constraints.
  static constexpr image_buffer_usage_t kCaptureUsage = {
      .tiling_type = IMAGE_TILING_TYPE_CAPTURE,
  };
  EXPECT_OK(display_engine_->DisplayEngineSetBufferCollectionConstraints(&kCaptureUsage,
                                                                         kBanjoBufferCollectionId));

  // Invalid import: invalid buffer collection ID.
  uint64_t capture_handle = 0;
  const uint64_t kBanjoInvalidBufferCollectionId = 100;
  EXPECT_EQ(display_engine_->DisplayEngineImportImageForCapture(kBanjoInvalidBufferCollectionId,
                                                                /*index=*/0, &capture_handle),
            ZX_ERR_NOT_FOUND);

  // Invalid import: index out of range.
  const uint64_t kInvalidIndex = 100;
  EXPECT_EQ(display_engine_->DisplayEngineImportImageForCapture(kBanjoBufferCollectionId,
                                                                kInvalidIndex, &capture_handle),
            ZX_ERR_OUT_OF_RANGE);

  // Valid import.
  capture_handle = 0;
  EXPECT_OK(display_engine_->DisplayEngineImportImageForCapture(kBanjoBufferCollectionId,
                                                                /*index=*/0, &capture_handle));
  EXPECT_NE(capture_handle, 0u);

  // Release the image.
  display_engine_->DisplayEngineReleaseCapture(capture_handle);

  EXPECT_OK(display_engine_->DisplayEngineReleaseBufferCollection(kBanjoBufferCollectionId));
}

TEST_F(FakeSysmemTest, SysmemRequirements) {
  MockBufferCollectionBase* collection = nullptr;
  allocator_->set_mock_buffer_collection_builder([&collection] {
    const std::vector<fuchsia_images2::wire::PixelFormat> kPixelFormats = {
        fuchsia_images2::wire::PixelFormat::kB8G8R8A8,
        fuchsia_images2::wire::PixelFormat::kR8G8B8A8};
    auto new_buffer_collection = std::make_unique<MockBufferCollection>(kPixelFormats);
    collection = new_buffer_collection.get();
    return new_buffer_collection;
  });

  auto [token_client, token_server] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();

  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  EXPECT_OK(display_engine_->DisplayEngineImportBufferCollection(kBanjoBufferCollectionId,
                                                                 token_client.TakeChannel()));

  EXPECT_TRUE(display::PollUntil([&] { return collection != nullptr; }, zx::msec(5), 1000));

  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(display_engine_->DisplayEngineSetBufferCollectionConstraints(&kDisplayUsage,
                                                                         kBanjoBufferCollectionId));

  EXPECT_TRUE(
      display::PollUntil([&] { return collection->set_constraints_called(); }, zx::msec(5), 1000));
  EXPECT_TRUE(collection->set_name_called());
}

TEST_F(FakeSysmemTest, SysmemRequirements_BgraOnly) {
  MockBufferCollectionBase* collection = nullptr;
  allocator_->set_mock_buffer_collection_builder([&collection] {
    const std::vector<fuchsia_images2::wire::PixelFormat> kPixelFormats = {
        fuchsia_images2::wire::PixelFormat::kB8G8R8A8,
    };
    auto new_buffer_collection = std::make_unique<MockBufferCollection>(kPixelFormats);
    collection = new_buffer_collection.get();
    return new_buffer_collection;
  });
  display_engine_->SetFormatSupportCheck([](fuchsia_images2::wire::PixelFormat format) {
    return format == fuchsia_images2::wire::PixelFormat::kB8G8R8A8;
  });

  auto [token_client, token_server] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();

  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  EXPECT_OK(display_engine_->DisplayEngineImportBufferCollection(kBanjoBufferCollectionId,
                                                                 token_client.TakeChannel()));

  EXPECT_TRUE(display::PollUntil([&] { return collection != nullptr; }, zx::msec(5), 1000));

  static constexpr image_buffer_usage_t kDisplayUsage = {
      .tiling_type = IMAGE_TILING_TYPE_LINEAR,
  };
  EXPECT_OK(display_engine_->DisplayEngineSetBufferCollectionConstraints(&kDisplayUsage,
                                                                         kBanjoBufferCollectionId));

  EXPECT_TRUE(
      display::PollUntil([&] { return collection->set_constraints_called(); }, zx::msec(5), 1000));
  EXPECT_TRUE(collection->set_name_called());
}

TEST(AmlogicDisplay, FloatToFix3_10) {
  inspect::Inspector inspector;
  EXPECT_EQ(0x0000u, VideoInputUnit::FloatToFixed3_10(0.0f));
  EXPECT_EQ(0x0066u, VideoInputUnit::FloatToFixed3_10(0.1f));
  EXPECT_EQ(0x1f9au, VideoInputUnit::FloatToFixed3_10(-0.1f));
  // Test for maximum positive (<4)
  EXPECT_EQ(0x0FFFu, VideoInputUnit::FloatToFixed3_10(4.0f));
  EXPECT_EQ(0x0FFFu, VideoInputUnit::FloatToFixed3_10(40.0f));
  EXPECT_EQ(0x0FFFu, VideoInputUnit::FloatToFixed3_10(3.9999f));
  // Test for minimum negative (>= -4)
  EXPECT_EQ(0x1000u, VideoInputUnit::FloatToFixed3_10(-4.0f));
  EXPECT_EQ(0x1000u, VideoInputUnit::FloatToFixed3_10(-14.0f));
}

TEST(AmlogicDisplay, FloatToFixed2_10) {
  inspect::Inspector inspector;
  EXPECT_EQ(0x0000u, VideoInputUnit::FloatToFixed2_10(0.0f));
  EXPECT_EQ(0x0066u, VideoInputUnit::FloatToFixed2_10(0.1f));
  EXPECT_EQ(0x0f9au, VideoInputUnit::FloatToFixed2_10(-0.1f));
  // Test for maximum positive (<2)
  EXPECT_EQ(0x07FFu, VideoInputUnit::FloatToFixed2_10(2.0f));
  EXPECT_EQ(0x07FFu, VideoInputUnit::FloatToFixed2_10(20.0f));
  EXPECT_EQ(0x07FFu, VideoInputUnit::FloatToFixed2_10(1.9999f));
  // Test for minimum negative (>= -2)
  EXPECT_EQ(0x0800u, VideoInputUnit::FloatToFixed2_10(-2.0f));
  EXPECT_EQ(0x0800u, VideoInputUnit::FloatToFixed2_10(-14.0f));
}

TEST_F(FakeSysmemTest, NoLeakCaptureCanvas) {
  allocator_->set_mock_buffer_collection_builder(
      [] { return std::make_unique<MockBufferCollectionForCapture>(); });

  auto [token_client, token_server] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollectionToken>::Create();

  constexpr display::DriverBufferCollectionId kBufferCollectionId(1);
  constexpr uint64_t kBanjoBufferCollectionId =
      display::ToBanjoDriverBufferCollectionId(kBufferCollectionId);
  EXPECT_OK(display_engine_->DisplayEngineImportBufferCollection(kBanjoBufferCollectionId,
                                                                 token_client.TakeChannel()));

  uint64_t capture_handle;
  EXPECT_OK(display_engine_->DisplayEngineImportImageForCapture(kBanjoBufferCollectionId,
                                                                /*index=*/0, &capture_handle));
  EXPECT_OK(display_engine_->DisplayEngineReleaseCapture(capture_handle));

  canvas_.SyncCall(&FakeCanvasProtocol::CheckThatNoEntriesInUse);
}

}  // namespace

}  // namespace amlogic_display
