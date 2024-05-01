// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/sysmem/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_creation_tokens.h>
#include <lib/ui/scenic/cpp/view_identity.h>

#include <cstdint>
#include <memory>

#include <zxtest/zxtest.h>

#include "src/ui/scenic/lib/allocation/buffer_collection_import_export_tokens.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/tests/utils/blocking_present.h"
#include "src/ui/scenic/tests/utils/logging_event_loop.h"
#include "src/ui/scenic/tests/utils/scenic_realm_builder.h"

namespace integration_tests {

using component_testing::RealmRoot;
using fuchsia::ui::composition::Allocator;
using fuchsia::ui::composition::Allocator_RegisterBufferCollection_Result;
using fuchsia::ui::composition::ContentId;
using fuchsia::ui::composition::Flatland;
using fuchsia::ui::composition::ImageProperties;
using fuchsia::ui::composition::RegisterBufferCollectionArgs;
using fuchsia::ui::composition::TransformId;

constexpr auto kDefaultSize = 128;
constexpr TransformId kRootTransform{.value = 1};

fuchsia::sysmem::BufferCollectionConstraints GetDefaultBufferConstraints() {
  fuchsia::sysmem::BufferCollectionConstraints constraints;
  constraints.has_buffer_memory_constraints = true;
  constraints.buffer_memory_constraints = {.ram_domain_supported = true,
                                           .cpu_domain_supported = true};
  constraints.usage = fuchsia::sysmem::BufferUsage{.cpu = fuchsia::sysmem::cpuUsageRead};
  constraints.min_buffer_count = 1;
  constraints.image_format_constraints_count = 1;
  auto& image_constraints = constraints.image_format_constraints[0];
  image_constraints.pixel_format.type = fuchsia::sysmem::PixelFormatType::BGRA32;
  image_constraints.pixel_format.has_format_modifier = true;
  image_constraints.pixel_format.format_modifier.value = fuchsia::sysmem::FORMAT_MODIFIER_LINEAR;
  image_constraints.color_spaces_count = 1;
  image_constraints.color_space[0].type = fuchsia::sysmem::ColorSpaceType::SRGB;
  image_constraints.required_min_coded_width = kDefaultSize;
  image_constraints.required_min_coded_height = kDefaultSize;
  image_constraints.required_max_coded_width = kDefaultSize;
  image_constraints.required_max_coded_height = kDefaultSize;

  return constraints;
}

// Test fixture that sets up an environment with a Scenic we can connect to.
class AllocationTest : public LoggingEventLoop, public zxtest::Test {
 public:
  void SetUp() override {
    // Build the realm topology and route the protocols required by this test fixture from the
    // scenic subrealm.
    realm_ = std::make_unique<RealmRoot>(ScenicRealmBuilder()
                                             .AddRealmProtocol(Flatland::Name_)
                                             .AddRealmProtocol(Allocator::Name_)
                                             .Build());

    auto context = sys::ComponentContext::Create();
    context->svc()->Connect(sysmem_allocator_.NewRequest());

    // Create a flatland display so render and cleanup loops happen.
    flatland_display_ = realm_->component().Connect<fuchsia::ui::composition::FlatlandDisplay>();
    flatland_display_.set_error_handler([](zx_status_t status) {
      FX_LOGS(ERROR) << "Lost connection to Scenic: " << zx_status_get_string(status);
      FAIL();
    });

    // Create a root Flatland.
    root_flatland_ = realm_->component().Connect<Flatland>();
    root_flatland_.set_error_handler([](zx_status_t status) {
      FX_LOGS(ERROR) << "Lost connection to Scenic: " << zx_status_get_string(status);
      FAIL();
    });

    // Attach |root_flatland_| as the only Flatland under |flatland_display_|.
    auto [child_token, parent_token] = scenic::ViewCreationTokenPair::New();
    fidl::InterfacePtr<fuchsia::ui::composition::ChildViewWatcher> child_view_watcher;
    flatland_display_->SetContent(std::move(parent_token), child_view_watcher.NewRequest());
    fidl::InterfacePtr<fuchsia::ui::composition::ParentViewportWatcher> parent_viewport_watcher;
    root_flatland_->CreateView2(std::move(child_token), scenic::NewViewIdentityOnCreation(), {},
                                parent_viewport_watcher.NewRequest());
    root_flatland_->CreateTransform(kRootTransform);
    root_flatland_->SetRootTransform(kRootTransform);
  }

  void TearDown() override {
    root_flatland_.Unbind();
    flatland_display_.Unbind();

    bool complete = false;
    realm_->Teardown([&](fit::result<fuchsia::component::Error> result) { complete = true; });
    RunLoopUntil([&]() { return complete; });

    zxtest::Test::TearDown();
  }

 protected:
  fuchsia::sysmem::BufferCollectionInfo_2 SetConstraintsAndAllocateBuffer(
      fuchsia::sysmem::BufferCollectionTokenSyncPtr token,
      fuchsia::sysmem::BufferCollectionConstraints constraints) {
    fuchsia::sysmem::BufferCollectionSyncPtr buffer_collection;
    auto status =
        sysmem_allocator_->BindSharedCollection(std::move(token), buffer_collection.NewRequest());
    FX_CHECK(status == ZX_OK);

    status = buffer_collection->SetConstraints(true, constraints);
    FX_CHECK(status == ZX_OK);
    zx_status_t allocation_status = ZX_OK;

    fuchsia::sysmem::BufferCollectionInfo_2 buffer_collection_info{};
    status =
        buffer_collection->WaitForBuffersAllocated(&allocation_status, &buffer_collection_info);
    FX_CHECK(status == ZX_OK);
    FX_CHECK(allocation_status == ZX_OK);
    EXPECT_EQ(constraints.min_buffer_count, buffer_collection_info.buffer_count);
    FX_CHECK(buffer_collection->Close() == ZX_OK);
    return buffer_collection_info;
  }

  fuchsia::sysmem::AllocatorSyncPtr sysmem_allocator_;
  std::unique_ptr<RealmRoot> realm_;
  fuchsia::ui::composition::FlatlandPtr root_flatland_;

 private:
  fuchsia::ui::composition::FlatlandDisplayPtr flatland_display_;
};

TEST_F(AllocationTest, CreateAndReleaseImage) {
  auto flatland_allocator = realm_->component().ConnectSync<Allocator>();

  auto [local_token, scenic_token] = utils::CreateSysmemTokens(sysmem_allocator_.get());

  // Send one token to Flatland Allocator.
  allocation::BufferCollectionImportExportTokens bc_tokens =
      allocation::BufferCollectionImportExportTokens::New();
  RegisterBufferCollectionArgs rbc_args = {};
  rbc_args.set_export_token(std::move(bc_tokens.export_token));
  rbc_args.set_buffer_collection_token(std::move(scenic_token));
  Allocator_RegisterBufferCollection_Result result;
  flatland_allocator->RegisterBufferCollection(std::move(rbc_args), &result);
  ASSERT_FALSE(result.is_err());

  // Use the local token to set constraints.
  auto info =
      SetConstraintsAndAllocateBuffer(std::move(local_token), GetDefaultBufferConstraints());

  ImageProperties image_properties = {};
  image_properties.set_size({.width = kDefaultSize, .height = kDefaultSize});
  const ContentId kImageContentId{.value = 1};

  root_flatland_->CreateImage(kImageContentId, std::move(bc_tokens.import_token), 0,
                              std::move(image_properties));
  root_flatland_->SetContent(kRootTransform, kImageContentId);
  BlockingPresent(this, root_flatland_);

  // Release image and remove content to actually deallocate.
  root_flatland_->ReleaseImage(kImageContentId);
  root_flatland_->SetContent(kRootTransform, {0});
  BlockingPresent(this, root_flatland_);
}

TEST_F(AllocationTest, CreateAndReleaseMultipleImages) {
  const auto kImageCount = 3;
  auto flatland_allocator = realm_->component().ConnectSync<Allocator>();

  for (uint64_t i = 1; i <= kImageCount; ++i) {
    auto [local_token, scenic_token] = utils::CreateSysmemTokens(sysmem_allocator_.get());

    // Send one token to root_flatland_ Allocator.
    allocation::BufferCollectionImportExportTokens bc_tokens =
        allocation::BufferCollectionImportExportTokens::New();
    RegisterBufferCollectionArgs rbc_args = {};
    rbc_args.set_export_token(std::move(bc_tokens.export_token));
    rbc_args.set_buffer_collection_token(std::move(scenic_token));
    Allocator_RegisterBufferCollection_Result result;
    flatland_allocator->RegisterBufferCollection(std::move(rbc_args), &result);
    ASSERT_FALSE(result.is_err());

    // Use the local token to set constraints.
    auto info =
        SetConstraintsAndAllocateBuffer(std::move(local_token), GetDefaultBufferConstraints());

    ImageProperties image_properties = {};
    image_properties.set_size({.width = kDefaultSize, .height = kDefaultSize});
    const ContentId kImageContentId{.value = i};
    root_flatland_->CreateImage(kImageContentId, std::move(bc_tokens.import_token), 0,
                                std::move(image_properties));
    const TransformId kImageTransformId{.value = i + 1};
    root_flatland_->CreateTransform(kImageTransformId);
    root_flatland_->SetContent(kImageTransformId, kImageContentId);
    root_flatland_->AddChild(kRootTransform, kImageTransformId);
  }
  BlockingPresent(this, root_flatland_);

  for (uint64_t i = 1; i <= kImageCount; ++i) {
    // Release image and remove content to actually deallocate.
    const ContentId kImageContentId{.value = i};
    root_flatland_->ReleaseImage(kImageContentId);
    const TransformId kImageTransformId{.value = i + 1};
    root_flatland_->RemoveChild(kRootTransform, kImageTransformId);
    root_flatland_->ReleaseTransform(kImageTransformId);
  }
  BlockingPresent(this, root_flatland_);
}

TEST_F(AllocationTest, MultipleClientsCreateAndReleaseImages) {
  const auto kClientCount = 16;

  // Add Viewports for as many as kClientCount.
  std::vector<fuchsia::ui::views::ViewCreationToken> view_creation_tokens;
  for (uint64_t i = 1; i <= kClientCount; ++i) {
    auto [child_token, parent_token] = scenic::ViewCreationTokenPair::New();
    view_creation_tokens.emplace_back(std::move(child_token));
    fidl::InterfacePtr<fuchsia::ui::composition::ChildViewWatcher> child_view_watcher;
    fuchsia::ui::composition::ViewportProperties properties;
    properties.set_logical_size({.width = kDefaultSize, .height = kDefaultSize});
    const ContentId kViewportContentId{.value = i};
    root_flatland_->CreateViewport(kViewportContentId, std::move(parent_token),
                                   std::move(properties), child_view_watcher.NewRequest());
    const TransformId kViewportTransformId{.value = i + 1};
    root_flatland_->CreateTransform(kViewportTransformId);
    root_flatland_->AddChild(kRootTransform, kViewportTransformId);
  }
  BlockingPresent(this, root_flatland_);

  std::vector<std::shared_ptr<async::Loop>> loops;
  for (uint64_t i = 0; i < kClientCount; ++i) {
    auto loop = std::make_shared<async::Loop>(&kAsyncLoopConfigNeverAttachToThread);
    loops.push_back(loop);
    auto status = loop->StartThread();
    EXPECT_EQ(status, ZX_OK);
    status = async::PostTask(loop->dispatcher(), [this, i, loop, &view_creation_tokens]() mutable {
      LoggingEventLoop present_loop;
      auto flatland_allocator = realm_->component().ConnectSync<Allocator>();

      auto [local_token, scenic_token] = utils::CreateSysmemTokens(sysmem_allocator_.get());

      // Send one token to Flatland Allocator.
      allocation::BufferCollectionImportExportTokens bc_tokens =
          allocation::BufferCollectionImportExportTokens::New();
      RegisterBufferCollectionArgs rbc_args = {};
      rbc_args.set_export_token(std::move(bc_tokens.export_token));
      rbc_args.set_buffer_collection_token(std::move(scenic_token));
      Allocator_RegisterBufferCollection_Result result;
      flatland_allocator->RegisterBufferCollection(std::move(rbc_args), &result);
      ASSERT_FALSE(result.is_err());

      // Use the local token to set constraints.
      auto info =
          SetConstraintsAndAllocateBuffer(std::move(local_token), GetDefaultBufferConstraints());

      auto flatland = realm_->component().Connect<Flatland>();
      flatland.set_error_handler([](zx_status_t status) {
        FX_LOGS(ERROR) << "Lost connection to Scenic: " << zx_status_get_string(status);
        FAIL();
      });
      fidl::InterfacePtr<fuchsia::ui::composition::ParentViewportWatcher> parent_viewport_watcher;
      flatland->CreateView(std::move(view_creation_tokens[i]),
                           parent_viewport_watcher.NewRequest());
      flatland->CreateTransform(kRootTransform);
      flatland->SetRootTransform(kRootTransform);

      ImageProperties image_properties;
      image_properties.set_size({.width = kDefaultSize, .height = kDefaultSize});
      const ContentId kImageContentId{.value = 1};
      flatland->CreateImage(kImageContentId, std::move(bc_tokens.import_token), 0,
                            std::move(image_properties));
      // Make each overlapping child slightly smaller, so all Images are visible.
      const auto size = kDefaultSize - static_cast<uint32_t>(i);
      flatland->SetImageDestinationSize(kImageContentId, {.width = size, .height = size});
      flatland->SetContent(kRootTransform, kImageContentId);
      BlockingPresent(&present_loop, flatland);

      // Release image and remove content to actually deallocate.
      flatland->ReleaseImage(kImageContentId);
      flatland->SetContent(kRootTransform, {0});
      BlockingPresent(&present_loop, flatland);

      flatland.Unbind();
      loop->Quit();
    });
    EXPECT_EQ(status, ZX_OK);
  }
  for (uint64_t i = 0; i < kClientCount; ++i) {
    loops[i]->JoinThreads();
  }
}

}  // namespace integration_tests