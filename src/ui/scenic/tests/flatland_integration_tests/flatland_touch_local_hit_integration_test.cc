// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/ui/composition/cpp/fidl.h>
#include <fuchsia/ui/pointer/augment/cpp/fidl.h>
#include <fuchsia/ui/pointer/cpp/fidl.h>
#include <fuchsia/ui/pointerinjector/cpp/fidl.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/ui/scenic/cpp/view_creation_tokens.h>
#include <lib/ui/scenic/cpp/view_identity.h>
#include <zircon/status.h>

#include <memory>
#include <vector>

#include <zxtest/zxtest.h>

#include "src/ui/scenic/tests/utils/logging_event_loop.h"
#include "src/ui/scenic/tests/utils/scenic_realm_builder.h"
#include "src/ui/scenic/tests/utils/utils.h"

// These tests exercise the integration between Flatland and the InputSystem for
// TouchSourceWithLocalHit. Setup:
// - Injection done in context View Space, with fuchsia.ui.pointerinjector
// - Target(s) specified by View (using view ref koids)
// - Dispatch done to fuchsia.ui.pointer.TouchSourceWithLocalHit in receiver(s') View Space.

// TODO(fxbug.dev/120775): Investigate GFX view tree getting tickled.

namespace integration_tests {

using ChildViewWatcher = fuchsia::ui::composition::ChildViewWatcher;
using ContentId = fuchsia::ui::composition::ContentId;
using Flatland = fuchsia::ui::composition::Flatland;
using FlatlandDisplay = fuchsia::ui::composition::FlatlandDisplay;
using FlatlandDisplayPtr = fuchsia::ui::composition::FlatlandDisplayPtr;
using FlatlandPtr = fuchsia::ui::composition::FlatlandPtr;
using ParentViewportWatcher = fuchsia::ui::composition::ParentViewportWatcher;
using ViewportProperties = fuchsia::ui::composition::ViewportProperties;
using TransformId = fuchsia::ui::composition::TransformId;
using ViewBoundProtocols = fuchsia::ui::composition::ViewBoundProtocols;

using Config = fuchsia::ui::pointerinjector::Config;
using Context = fuchsia::ui::pointerinjector::Context;
using Data = fuchsia::ui::pointerinjector::Data;
using DevicePtr = fuchsia::ui::pointerinjector::DevicePtr;
using DeviceType = fuchsia::ui::pointerinjector::DeviceType;
using DispatchPolicy = fuchsia::ui::pointerinjector::DispatchPolicy;
using Event = fuchsia::ui::pointerinjector::Event;
using EventPhase = fuchsia::ui::pointerinjector::EventPhase;
using PointerSample = fuchsia::ui::pointerinjector::PointerSample;
using Target = fuchsia::ui::pointerinjector::Target;
using Viewport = fuchsia::ui::pointerinjector::Viewport;

using TouchEventWithLocalHit = fuchsia::ui::pointer::augment::TouchEventWithLocalHit;

using TouchResponse = fuchsia::ui::pointer::TouchResponse;
using TouchResponseType = fuchsia::ui::pointer::TouchResponseType;
using TouchSource = fuchsia::ui::pointer::TouchSource;
using TouchSourcePtr = fuchsia::ui::pointer::TouchSourcePtr;

using ViewRef = fuchsia::ui::views::ViewRef;

using RealmRoot = component_testing::RealmRoot;

class FlatlandTouchLocalHitIntegrationTest : public zxtest::Test, public LoggingEventLoop {
 protected:
  static constexpr uint32_t kDeviceId = 1111;
  static constexpr uint32_t kPointerId = 2222;

  // clang-format off
  static constexpr std::array<float, 9> kIdentityMatrix = {
    1, 0, 0, // column one
    0, 1, 0, // column two
    0, 0, 1, // column three
  };
  // clang-format on

  void SetUp() override {
    // Build the realm topology and route the protocols required by this test fixture from the
    // scenic subrealm.
    realm_ = std::make_unique<RealmRoot>(
        ScenicRealmBuilder({.use_flatland = false})
            .AddRealmProtocol(fuchsia::ui::composition::Flatland::Name_)
            .AddRealmProtocol(fuchsia::ui::composition::FlatlandDisplay::Name_)
            .AddRealmProtocol(fuchsia::ui::composition::Allocator::Name_)
            .AddRealmProtocol(fuchsia::ui::pointerinjector::Registry::Name_)
            .Build());

    flatland_display_ = realm_->component().Connect<FlatlandDisplay>();
    flatland_display_.set_error_handler([](zx_status_t status) {
      FAIL("Lost connection to Scenic: %s", zx_status_get_string(status));
    });

    pointerinjector_registry_ =
        realm_->component().Connect<fuchsia::ui::pointerinjector::Registry>();
    pointerinjector_registry_.set_error_handler([](zx_status_t status) {
      FAIL("Lost connection to pointerinjector Registry: %s", zx_status_get_string(status));
    });

    local_hit_registry_ = realm_->component().Connect<fuchsia::ui::pointer::augment::LocalHit>();
    local_hit_registry_.set_error_handler([](zx_status_t status) {
      FAIL("Lost connection to LocalHit Registry: %s", zx_status_get_string(status));
    });

    // Set up root view and root transform.
    root_instance_ = realm_->component().Connect<Flatland>();
    root_instance_.set_error_handler([](zx_status_t status) {
      FAIL("Lost connection to Scenic: %s", zx_status_get_string(status));
    });

    auto [child_token, parent_token] = scenic::ViewCreationTokenPair::New();
    fidl::InterfacePtr<ParentViewportWatcher> parent_viewport_watcher;
    auto identity = scenic::NewViewIdentityOnCreation();
    root_view_ref_ = fidl::Clone(identity.view_ref);

    root_instance_->CreateView2(std::move(child_token), std::move(identity),
                                /*view bound protocols*/ {}, parent_viewport_watcher.NewRequest());

    fidl::InterfacePtr<ChildViewWatcher> child_view_watcher;
    flatland_display_->SetContent(std::move(parent_token), child_view_watcher.NewRequest());

    root_instance_->CreateTransform(kRootTransform);
    root_instance_->SetRootTransform(kRootTransform);
    BlockingPresent(root_instance_);

    // Get the display's width and height. Since there is no Present in FlatlandDisplay, receiving
    // this callback ensures that all |flatland_display_| calls are processed.
    std::optional<fuchsia::ui::composition::LayoutInfo> info;
    parent_viewport_watcher->GetLayout([&info](auto result) { info = std::move(result); });
    RunLoopUntil([&info] { return info.has_value(); });
    display_width_ = static_cast<float>(info->logical_size().width);
    display_height_ = static_cast<float>(info->logical_size().height);
  }

  void BlockingPresent(FlatlandPtr& flatland) {
    bool presented = false;
    flatland.events().OnFramePresented = [&presented](auto) { presented = true; };
    flatland->Present({});
    RunLoopUntil([&presented] { return presented; });
    flatland.events().OnFramePresented = nullptr;
  }

  void RegisterInjector(ViewRef context_view_ref, ViewRef target_view_ref) {
    Config config;
    config.set_device_id(kDeviceId);
    config.set_device_type(DeviceType::TOUCH);
    config.set_dispatch_policy(DispatchPolicy::TOP_HIT_AND_ANCESTORS_IN_TARGET);

    {
      {
        Context context;
        context.set_view(std::move(context_view_ref));
        config.set_context(std::move(context));
      }
      {
        Target target;
        target.set_view(std::move(target_view_ref));
        config.set_target(std::move(target));
      }
      {
        Viewport viewport;
        viewport.set_extents(FullScreenExtents());
        viewport.set_viewport_to_context_transform(kIdentityMatrix);
        config.set_viewport(std::move(viewport));
      }
    }

    injector_.set_error_handler([this](zx_status_t) { injector_channel_closed_ = true; });
    bool register_callback_fired = false;
    pointerinjector_registry_->Register(
        std::move(config), injector_.NewRequest(),
        [&register_callback_fired] { register_callback_fired = true; });

    RunLoopUntil([&register_callback_fired] { return register_callback_fired; });
    ASSERT_FALSE(injector_channel_closed_);
  }

  void Inject(float x, float y, EventPhase phase) {
    FX_CHECK(injector_);
    Event event;
    event.set_timestamp(0);
    {
      PointerSample pointer_sample;
      pointer_sample.set_pointer_id(kPointerId);
      pointer_sample.set_phase(phase);
      pointer_sample.set_position_in_viewport({x, y});
      Data data;
      data.set_pointer_sample(std::move(pointer_sample));
      event.set_data(std::move(data));
    }
    std::vector<Event> events;
    events.emplace_back(std::move(event));
    bool hanging_get_returned = false;
    injector_->Inject(std::move(events), [&hanging_get_returned] { hanging_get_returned = true; });
    RunLoopUntil(
        [this, &hanging_get_returned] { return hanging_get_returned || injector_channel_closed_; });
  }

  // Starts a recursive TouchSource::Watch() loop that collects all received events into
  // |out_events|.
  void StartWatchLoop(fuchsia::ui::pointer::augment::TouchSourceWithLocalHitPtr& touch_source,
                      std::vector<TouchEventWithLocalHit>& out_events,
                      TouchResponseType response_type = TouchResponseType::MAYBE) {
    const size_t index = watch_loops_.size();
    watch_loops_.emplace_back();
    watch_loops_.at(index) = [this, &touch_source, &out_events, response_type, index](auto events) {
      std::vector<TouchResponse> responses;
      for (auto& event : events) {
        if (event.touch_event.has_pointer_sample()) {
          TouchResponse response;
          response.set_response_type(response_type);
          responses.emplace_back(std::move(response));
        } else {
          responses.emplace_back();
        }
      }
      std::move(events.begin(), events.end(), std::back_inserter(out_events));

      touch_source->Watch(std::move(responses), [this, index](auto events) {
        watch_loops_.at(index)(std::move(events));
      });
    };
    touch_source->Watch({}, watch_loops_.at(index));
  }

  // Convenience function, we assume the test constructs topologies with one level of N children.
  // Prereq: |parent_of_viewport_transform| is created and connected to the view's root.
  ViewRef CreateAndAddChildView(FlatlandPtr& parent_instance, TransformId viewport_transform_id,
                                fuchsia::math::Rect viewport_spec,
                                TransformId parent_of_viewport_transform,
                                ContentId viewport_content_id, FlatlandPtr& child_instance,
                                fidl::InterfaceRequest<TouchSource> child_touch_source = nullptr) {
    child_instance = realm_->component().Connect<Flatland>();

    // Set up the child view watcher.
    fidl::InterfacePtr<ChildViewWatcher> child_view_watcher;
    auto [child_token, parent_token] = scenic::ViewCreationTokenPair::New();
    ViewportProperties properties;
    properties.set_logical_size({.width = static_cast<uint32_t>(viewport_spec.width),
                                 .height = static_cast<uint32_t>(viewport_spec.height)});

    parent_instance->CreateTransform(viewport_transform_id);
    parent_instance->CreateViewport(viewport_content_id, std::move(parent_token),
                                    std::move(properties), child_view_watcher.NewRequest());
    parent_instance->SetContent(viewport_transform_id, viewport_content_id);
    parent_instance->AddChild(parent_of_viewport_transform, viewport_transform_id);
    parent_instance->SetTranslation(viewport_transform_id,
                                    {.x = viewport_spec.x, .y = viewport_spec.y});

    BlockingPresent(parent_instance);

    // Set up the child view along with its TouchSource channel.
    fidl::InterfacePtr<ParentViewportWatcher> parent_viewport_watcher;
    auto identity = scenic::NewViewIdentityOnCreation();
    auto child_view_ref = fidl::Clone(identity.view_ref);
    ViewBoundProtocols protocols;
    if (child_touch_source) {
      protocols.set_touch_source(std::move(child_touch_source));
    }
    child_instance->CreateView2(std::move(child_token), std::move(identity), std::move(protocols),
                                parent_viewport_watcher.NewRequest());
    child_instance->CreateTransform(kRootTransform);
    child_instance->SetRootTransform(kRootTransform);
    BlockingPresent(child_instance);

    return child_view_ref;
  }

  std::array<std::array<float, 2>, 2> FullScreenExtents() const {
    return {{{0, 0}, {display_width_, display_height_}}};
  }

  static constexpr TransformId kRootTransform{.value = 1};
  static constexpr ContentId kRootContentId{.value = 1};

  FlatlandPtr root_instance_;

  ViewRef root_view_ref_;

  bool injector_channel_closed_ = false;

  fuchsia::ui::pointer::augment::LocalHitPtr local_hit_registry_;

  float display_width_ = 0;

  float display_height_ = 0;

  std::unique_ptr<RealmRoot> realm_;

 private:
  FlatlandDisplayPtr flatland_display_;

  fuchsia::ui::pointerinjector::RegistryPtr pointerinjector_registry_;

  DevicePtr injector_;

  // Holds watch loops so they stay alive through the duration of the test.
  std::vector<std::function<void(std::vector<TouchEventWithLocalHit>)>> watch_loops_;
};

// In this test we set up three views beneath the root, 1 and its two children 2 and 3, giving us
// the following topology:
//    1
//   / \
//  2   3
// Each view is sized as follows:
// - view 1 is 7x7 pixels
// - view 2 is 3x3 pixels
// - view 3 is 3x3 pixels
// Each view has a default hit region covering their entire View.
// Each view is placed as follows: View 3 is placed above View 2, itself above View 1.
//
// To exercise hit testing, the test performs a touch gesture diagonally across all views and
// observe that the expected local hits are delivered. The display is much larger than view 1, and
// we only exercise the top 8x8 region of the display.
//
// 1: View 1, 2: View 2, 3: View 3, x: No view, []: touch point
//
//   X ->
// Y [1] 1  1  1  1  1  1  x
// |  1 [2] 2  2  1  1  1  x
// v  1  2 [2] 2  1  1  1  x
//    1  2  2 [3] 3  3  1  x
//    1  1  1  3 [3] 3  1  x
//    1  1  1  3  3 [3] 1  x
//    1  1  1  1  1  1 [1] x
//    x  x  x  x  x  x  x [x]
//
TEST_F(FlatlandTouchLocalHitIntegrationTest, InjectedInput_ShouldBeCorrectlyTransformed) {
  // Create View 1
  FlatlandPtr view1_instance;
  TouchSourcePtr view1_touch_source;
  view1_instance.set_error_handler([](zx_status_t status) {
    FAIL("Lost connection to Scenic: %s", zx_status_get_string(status));
  });
  view1_touch_source.set_error_handler([](zx_status_t status) {
    FAIL("Touch source closed with status: %s", zx_status_get_string(status));
  });
  const auto view1_ref = CreateAndAddChildView(
      root_instance_, /*viewport_transform_id*/ {.value = 2},
      /*viewport_spec*/ {.x = 0, .y = 0, .width = 7, .height = 7},
      /*parent_of_viewport_transform*/ kRootTransform,
      /*viewport_content_id*/ {.value = 2}, view1_instance, view1_touch_source.NewRequest());
  const auto view1_koid = ExtractKoid(view1_ref);

  // Create View 2
  FlatlandPtr view2_instance;
  view2_instance.set_error_handler([](zx_status_t status) {
    FAIL("Lost connection to Scenic: %s", zx_status_get_string(status));
  });

  const auto view2_ref =
      CreateAndAddChildView(view1_instance, /*viewport_transform_id*/ {.value = 2},
                            /*viewport_spec*/ {.x = 1, .y = 1, .width = 3, .height = 3},
                            /*parent_of_viewport_transform*/ kRootTransform,
                            /*viewport_content_id*/ {.value = 2}, view2_instance);
  const auto view2_koid = ExtractKoid(view2_ref);

  // Create View 3
  FlatlandPtr view3_instance;
  view3_instance.set_error_handler([](zx_status_t status) {
    FAIL("Lost connection to Scenic: %s", zx_status_get_string(status));
  });

  const auto view3_ref =
      CreateAndAddChildView(view1_instance, /*viewport_transform_id*/ {.value = 3},
                            /*viewport_spec*/ {.x = 3, .y = 3, .width = 3, .height = 3},
                            /*parent_of_viewport_transform*/ kRootTransform,
                            /*viewport_content_id*/ {.value = 3}, view3_instance);
  const auto view3_koid = ExtractKoid(view3_ref);

  BlockingPresent(view1_instance);
  BlockingPresent(view2_instance);
  BlockingPresent(view3_instance);

  // Upgrade View 1's touch source
  fuchsia::ui::pointer::augment::TouchSourceWithLocalHitPtr touch_source_with_local_hit;
  local_hit_registry_->Upgrade(std::move(view1_touch_source),
                               [&touch_source_with_local_hit](auto upgraded, auto error) {
                                 EXPECT_TRUE(upgraded);
                                 ASSERT_FALSE(error);
                                 touch_source_with_local_hit = upgraded.Bind();
                               });
  RunLoopUntil([&touch_source_with_local_hit] { return touch_source_with_local_hit.is_bound(); });

  std::vector<TouchEventWithLocalHit> child_events;
  StartWatchLoop(touch_source_with_local_hit, child_events);

  // Begin test.
  RegisterInjector(fidl::Clone(root_view_ref_), fidl::Clone(view1_ref));
  Inject(0.5f, 0.5f, EventPhase::ADD);
  Inject(1.5f, 1.5f, EventPhase::CHANGE);
  Inject(2.5f, 2.5f, EventPhase::CHANGE);
  Inject(3.5f, 3.5f, EventPhase::CHANGE);
  Inject(4.5f, 4.5f, EventPhase::CHANGE);
  Inject(5.5f, 5.5f, EventPhase::CHANGE);
  Inject(6.5f, 6.5f, EventPhase::CHANGE);
  Inject(7.5f, 7.5f, EventPhase::REMOVE);
  RunLoopUntil([&child_events] { return child_events.size() == 8u; });  // Succeeds or times out.

  EXPECT_EQ(child_events.at(0).local_viewref_koid, view1_koid);
  EXPECT_EQ(child_events.at(1).local_viewref_koid, view2_koid);
  EXPECT_EQ(child_events.at(2).local_viewref_koid, view2_koid);
  EXPECT_EQ(child_events.at(3).local_viewref_koid, view3_koid);
  EXPECT_EQ(child_events.at(4).local_viewref_koid, view3_koid);
  EXPECT_EQ(child_events.at(5).local_viewref_koid, view3_koid);
  EXPECT_EQ(child_events.at(6).local_viewref_koid, view1_koid);
  EXPECT_EQ(child_events.at(7).local_viewref_koid, ZX_KOID_INVALID);  // No View

  EXPECT_EQ(child_events.at(0).local_point[0], 0.5f);  // View 1
  EXPECT_EQ(child_events.at(1).local_point[0], 0.5f);  // View 2
  EXPECT_EQ(child_events.at(2).local_point[0], 1.5f);  // View 2
  EXPECT_EQ(child_events.at(3).local_point[0], 0.5f);  // View 3
  EXPECT_EQ(child_events.at(4).local_point[0], 1.5f);  // View 3
  EXPECT_EQ(child_events.at(5).local_point[0], 2.5f);  // View 3
  EXPECT_EQ(child_events.at(6).local_point[0], 6.5f);  // View 1
  EXPECT_EQ(child_events.at(7).local_point[0], 0.0f);  // No View
}

}  // namespace integration_tests
