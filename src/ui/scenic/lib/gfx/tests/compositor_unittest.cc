// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <lib/ui/scenic/cpp/commands.h>
#include <zircon/syscalls.h>

#include <thread>

#include <gtest/gtest.h>

#include "src/ui/lib/escher/test/common/gtest_vulkan.h"
#include "src/ui/scenic/lib/display/display_manager.h"
#include "src/ui/scenic/lib/display/tests/mock_display_controller.h"
#include "src/ui/scenic/lib/gfx/swapchain/display_swapchain.h"
#include "src/ui/scenic/lib/gfx/tests/vk_session_test.h"
#include "src/ui/scenic/lib/gfx/util/time.h"

namespace scenic_impl::gfx::test {

namespace {

fidl::Endpoints<fuchsia_hardware_display::Coordinator> CreateCoordinatorEndpoints() {
  zx::result<fidl::Endpoints<fuchsia_hardware_display::Coordinator>> endpoints_result =
      fidl::CreateEndpoints<fuchsia_hardware_display::Coordinator>();
  FX_CHECK(endpoints_result.is_ok())
      << "Failed to create endpoints: " << endpoints_result.status_string();
  return std::move(endpoints_result.value());
}

class CompositorTest : public SessionTest {
 public:
  CompositorTest() = default;

  void SetUp() override {
    SessionTest::SetUp();
    display_manager_ = std::make_unique<display::DisplayManager>([]() {});

    constexpr float display_width = 1024;
    constexpr float display_height = 768;
    display_manager_->SetDefaultDisplayForTests(std::make_unique<display::Display>(
        /*id*/ 0, /*px-width*/ display_width, /*px-height*/ display_height));
    sysmem_ = std::make_unique<Sysmem>();
  }

  void TearDown() override {
    SessionTest::TearDown();

    scene_graph_.reset();
    display_manager_.reset();
    sysmem_.reset();
  }

  SessionContext CreateSessionContext() override {
    SessionContext session_context = SessionTest::CreateSessionContext();

    FX_DCHECK(!scene_graph_);

    // Generate scene graph.
    scene_graph_ = std::make_unique<SceneGraph>();

    // Finally apply scene graph weak pointer.
    session_context.scene_graph = scene_graph_->GetWeakPtr();

    // Return session
    return session_context;
  }

  CommandContext CreateCommandContext() override {
    return {.sysmem = sysmem_.get(),
            .display_manager = display_manager_.get(),
            .warm_pipeline_cache_callback = [](vk::Format) {},
            .scene_graph = scene_graph_->GetWeakPtr()};
  }

  display::DisplayManager* display_manager() const { return display_manager_.get(); }

 private:
  std::unique_ptr<Sysmem> sysmem_;
  fuchsia::hardware::display::CoordinatorSyncPtr display_coordinator_;
  std::unique_ptr<display::DisplayManager> display_manager_;
  sys::testing::ComponentContextProvider context_provider_;
  std::unique_ptr<SceneGraph> scene_graph_;
};

TEST_F(CompositorTest, Validation) {
  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator_endpoints =
      CreateCoordinatorEndpoints();

  display_manager()->BindDefaultDisplayCoordinator(std::move(coordinator_endpoints.client));

  std::array<float, 3> preoffsets = {0, 0, 0};
  std::array<float, 9> matrix = {0.3, 0.6, 0.1, 0.3, 0.6, 0.1, 0.3, 0.6, 0.1};
  std::array<float, 3> postoffsets = {0, 0, 0};

  // Create a compositor
  const int CompositorId = 15;
  ASSERT_TRUE(Apply(scenic::NewCreateDisplayCompositorCmd(CompositorId)));

  // Create a mock display coordinator that runs on a separate thread.
  fidl::ServerEnd coordinator_server = std::move(coordinator_endpoints.server);
  std::thread server([&preoffsets, &matrix, &postoffsets,
                      coordinator_server = std::move(coordinator_server)]() mutable {
    async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

    display::test::MockDisplayCoordinator mock_display_coordinator;

    mock_display_coordinator.set_display_color_conversion_fn(
        [&](uint64_t display_id, std::array<float, 3> preoffsets_out,
            std::array<float, 9> matrix_out, std::array<float, 3> postoffsets_out) {
          // Check that the display coordinator got the color correction matrix we passed in.
          EXPECT_EQ(preoffsets, preoffsets_out);
          EXPECT_EQ(matrix, matrix_out);
          EXPECT_EQ(postoffsets, postoffsets_out);
        });
    mock_display_coordinator.Bind(coordinator_server.TakeChannel());

    // Waits for initial call to |SetOnVsyncCallback| by DisplayManager.
    mock_display_coordinator.WaitForMessage();

    // Waits for a call to |SetDisplayColorConversion| by client.
    EXPECT_EQ(0U, mock_display_coordinator.set_display_color_conversion_count());
    mock_display_coordinator.WaitForMessage();
    EXPECT_EQ(1U, mock_display_coordinator.set_display_color_conversion_count());

    // Wait for |CheckConfig|.
    EXPECT_EQ(0U, mock_display_coordinator.check_config_count());
    mock_display_coordinator.WaitForMessage();
    EXPECT_EQ(1U, mock_display_coordinator.check_config_count());
  });

  EXPECT_TRUE(Apply(
      scenic::NewSetDisplayColorConversionCmdHACK(CompositorId, preoffsets, matrix, postoffsets)));

  server.join();
}

// Test to make sure that we can set the minimum RGB value for the display via the
// standard GFX API, across a fidl channel.
TEST_F(CompositorTest, ValidateMinimumRGB) {
  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator_endpoints =
      CreateCoordinatorEndpoints();

  display_manager()->BindDefaultDisplayCoordinator(std::move(coordinator_endpoints.client));

  // Create a mock display coordinator that runs on a separate thread.
  uint8_t minimum = 10;
  fidl::ServerEnd coordinator_server = std::move(coordinator_endpoints.server);
  std::thread server([&minimum, coordinator_server = std::move(coordinator_server)]() mutable {
    async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

    display::test::MockDisplayCoordinator mock_display_coordinator;

    mock_display_coordinator.set_minimum_rgb_fn([&](uint8_t minimum_out) {
      // Check that the display coordinator got the right value.
      EXPECT_EQ(minimum, minimum_out);
    });
    mock_display_coordinator.Bind(coordinator_server.TakeChannel());

    // Waits for initial call to |SetOnVsyncCallback| by DisplayManager.
    mock_display_coordinator.WaitForMessage();

    // Waits for a call to |SetDisplayMinimumRgb| by client.
    EXPECT_EQ(0U, mock_display_coordinator.set_minimum_rgb_count());
    mock_display_coordinator.WaitForMessage();
    EXPECT_EQ(1U, mock_display_coordinator.set_minimum_rgb_count());

    // Wait for |CheckConfig|.
    EXPECT_EQ(0U, mock_display_coordinator.check_config_count());
    mock_display_coordinator.WaitForMessage();
    EXPECT_EQ(1U, mock_display_coordinator.check_config_count());
  });

  EXPECT_TRUE(Apply(scenic::NewSetDisplayMinimumRgbCmdHACK(minimum)));

  server.join();
}

using CompositorTestSimple = gtest::TestLoopFixture;

TEST_F(CompositorTestSimple, ColorConversionConfigChecking) {
  fuchsia::hardware::display::CoordinatorSyncPtr display_coordinator;
  display::test::MockDisplayCoordinator mock_display_coordinator;

  fidl::Endpoints<fuchsia_hardware_display::Coordinator> coordinator_endpoints =
      CreateCoordinatorEndpoints();

  mock_display_coordinator.Bind(coordinator_endpoints.server.TakeChannel());
  display_coordinator.Bind(coordinator_endpoints.client.TakeChannel());

  ColorTransform transform;

  uint32_t check_config_call_count = 0;
  bool should_discard_config = false;
  auto check_config_fn = [&](bool discard, fuchsia::hardware::display::ConfigResult* result,
                             std::vector<fuchsia::hardware::display::ClientCompositionOp>* ops) {
    *result = fuchsia::hardware::display::ConfigResult::UNSUPPORTED_CONFIG;

    fuchsia::hardware::display::ClientCompositionOp op;
    op.opcode = fuchsia::hardware::display::ClientCompositionOpcode::CLIENT_COLOR_CONVERSION;
    ops->push_back(op);
    check_config_call_count++;
    if (discard) {
      should_discard_config = true;
    }
  };
  mock_display_coordinator.set_check_config_fn(check_config_fn);

  std::thread client([display_coordinator = std::move(display_coordinator), transform]() mutable {
    DisplaySwapchain::SetDisplayColorConversion(/*id=*/1, display_coordinator, transform);
  });

  // Wait for |SetDisplayColorConversion|.
  mock_display_coordinator.WaitForMessage();

  // Wait for |CheckConfig|.
  mock_display_coordinator.WaitForMessage();

  // Wait for |CheckConfig|.
  mock_display_coordinator.WaitForMessage();

  client.join();

  // The function check_config_fn should be called twice, once for the
  // initial config check, and once with the |discard| variable set to true.
  EXPECT_EQ(check_config_call_count, 2U);
  EXPECT_TRUE(should_discard_config);
}

}  // namespace

}  // namespace scenic_impl::gfx::test
