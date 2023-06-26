// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_BIN_APP_H_
#define SRC_UI_SCENIC_BIN_APP_H_

#include <lib/async/cpp/executor.h>

#include <memory>
#include <optional>

#include "src/graphics/display/lib/coordinator-getter/client.h"
#include "src/lib/async-watchdog/watchdog.h"
#include "src/lib/fsl/io/device_watcher.h"
#include "src/ui/lib/escher/escher.h"
#include "src/ui/scenic/lib/allocation/allocator.h"
#include "src/ui/scenic/lib/annotation/annotation_registry.h"
#include "src/ui/scenic/lib/display/color_converter.h"
#include "src/ui/scenic/lib/display/display_manager.h"
#include "src/ui/scenic/lib/display/display_power_manager.h"
#include "src/ui/scenic/lib/display/singleton_display_service.h"
#include "src/ui/scenic/lib/flatland/engine/display_compositor.h"
#include "src/ui/scenic/lib/flatland/engine/engine.h"
#include "src/ui/scenic/lib/flatland/engine/engine_types.h"
#include "src/ui/scenic/lib/flatland/flatland_manager.h"
#include "src/ui/scenic/lib/flatland/flatland_presenter_impl.h"
#include "src/ui/scenic/lib/flatland/link_system.h"
#include "src/ui/scenic/lib/flatland/uber_struct_system.h"
#include "src/ui/scenic/lib/focus/focus_manager.h"
#include "src/ui/scenic/lib/gfx/engine/engine.h"
#include "src/ui/scenic/lib/input/input_system.h"
#include "src/ui/scenic/lib/scenic/scenic.h"
#include "src/ui/scenic/lib/scheduling/default_frame_scheduler.h"
#include "src/ui/scenic/lib/screen_capture/screen_capture_manager.h"
#include "src/ui/scenic/lib/screen_capture2/screen_capture2_manager.h"
#include "src/ui/scenic/lib/screenshot/screenshot_manager.h"
#include "src/ui/scenic/lib/shutdown/shutdown_manager.h"
#include "src/ui/scenic/lib/utils/cleanup_until_done.h"
#include "src/ui/scenic/lib/utils/metrics_impl.h"
#include "src/ui/scenic/lib/view_tree/geometry_provider.h"
#include "src/ui/scenic/lib/view_tree/observer_registry.h"
#include "src/ui/scenic/lib/view_tree/scoped_observer_registry.h"
#include "src/ui/scenic/lib/view_tree/view_ref_installed_impl.h"
#include "src/ui/scenic/lib/view_tree/view_tree_snapshotter.h"
namespace scenic_impl {

class DisplayInfoDelegate : public Scenic::GetDisplayInfoDelegateDeprecated {
 public:
  explicit DisplayInfoDelegate(std::shared_ptr<display::Display> display);

  // TODO(fxbug.dev/23686): Remove this when we externalize Displays.
  // |Scenic::GetDisplayInfoDelegateDeprecated|
  void GetDisplayInfo(fuchsia::ui::scenic::Scenic::GetDisplayInfoCallback callback) override;
  // |Scenic::GetDisplayInfoDelegateDeprecated|
  void GetDisplayOwnershipEvent(
      fuchsia::ui::scenic::Scenic::GetDisplayOwnershipEventCallback callback) override;

  fuchsia::math::SizeU GetDisplayDimensions();

 private:
  std::shared_ptr<display::Display> display_ = nullptr;
};

// Values read from config file. Set to their default values.
struct ConfigValues {
  zx::duration min_predicted_frame_duration =
      scheduling::DefaultFrameScheduler::kMinPredictedFrameDuration;
#if defined(USE_FLATLAND_BY_DEFAULT)
  bool i_can_haz_flatland = true;
#else
  bool i_can_haz_flatland = false;
#endif  // USE_FLATLAND_BY_DEFAULT
  bool enable_allocator_for_flatland = true;
  bool pointer_auto_focus_on = true;
#ifdef FLATLAND_ENABLE_DISPLAY_COMPOSITION
  bool flatland_enable_display_composition = true;
#else
  // Uses the GPU/Vulkan compositor by default, instead of attempting to composite using the display
  // coordinator.
  bool flatland_enable_display_composition = false;
#endif  // FLATLAND_DISABLE_DISPLAY_COMPOSITION
  // TODO(fxb/76985): Remove these when we have proper multi-display support.
  std::optional<uint64_t> i_can_haz_display_id;
  std::optional<size_t> i_can_haz_display_mode;

  // Angle in degrees by which the display is rotated in clockwise direction. This value is
  // read from `/config/data/display_rotation`. Must be equal to the |display_rotation| config
  // provided to the scene manager.
  uint32_t display_rotation = 0;
};

class App {
 public:
  App(std::unique_ptr<sys::ComponentContext> app_context, inspect::Node inspect_node,
      fpromise::promise<::display::CoordinatorClientEnd, zx_status_t> dc_handles_promise,
      fit::closure quit_callback);

  ~App();

 private:
  void InitializeServices(escher::EscherUniquePtr escher,
                          std::shared_ptr<display::Display> display);
  void InitializeGraphics(std::shared_ptr<display::Display> display);
  void InitializeInput();
  void InitializeHeartbeat(display::Display& display);

  async::Executor executor_;
  std::unique_ptr<sys::ComponentContext> app_context_;
  const ConfigValues config_values_;

  std::shared_ptr<ShutdownManager> shutdown_manager_;
  metrics::MetricsImpl metrics_logger_;
  inspect::Node inspect_node_;

  // FrameScheduler must be initialized early, since it must outlive all its
  // dependencies.
  scheduling::DefaultFrameScheduler frame_scheduler_;

  gfx::Sysmem sysmem_;
  std::optional<display::DisplayManager> display_manager_;
  std::optional<display::SingletonDisplayService> singleton_display_service_;
  std::optional<DisplayInfoDelegate> display_info_delegate_;
  // DisplayPowerManager has a reference to |display_manager_|, so it should be
  // destroyed before |display_manager_|.
  std::optional<display::DisplayPowerManager> display_power_manager_;
  escher::EscherUniquePtr escher_;
  std::shared_ptr<utils::CleanupUntilDone> escher_cleanup_;

  std::shared_ptr<gfx::ImagePipeUpdater> image_pipe_updater_;
  std::optional<gfx::Engine> engine_;
  Scenic scenic_;
  std::unique_ptr<fsl::DeviceWatcher> device_watcher_;

  std::shared_ptr<allocation::Allocator> allocator_;

  std::shared_ptr<flatland::UberStructSystem> uber_struct_system_;
  std::shared_ptr<flatland::LinkSystem> link_system_;
  std::shared_ptr<flatland::FlatlandPresenterImpl> flatland_presenter_;
  std::shared_ptr<flatland::FlatlandManager> flatland_manager_;
  std::shared_ptr<flatland::DisplayCompositor> flatland_compositor_;
  std::shared_ptr<flatland::Engine> flatland_engine_;

  display::ColorConverter color_converter_;

  std::optional<input::InputSystem> input_;
  focus::FocusManager focus_manager_;
  std::optional<view_tree::ViewTreeSnapshotter> view_tree_snapshotter_;
  std::optional<screen_capture::ScreenCaptureManager> screen_capture_manager_;
  std::optional<screen_capture2::ScreenCapture2Manager> screen_capture2_manager_;
  std::optional<screenshot::ScreenshotManager> screenshot_manager_;

  view_tree::ViewRefInstalledImpl view_ref_installed_impl_;

  view_tree::GeometryProvider geometry_provider_;
  view_tree::Registry observer_registry_;
  view_tree::ScopedRegistry scoped_observer_registry_;

  uint64_t gfx_frame_count_ = 0;
  uint64_t flatland_frame_count_ = 0;

  AnnotationRegistry annotation_registry_;

  async_watchdog::Watchdog watchdog_;

  const bool enable_snapshot_dump_ = false;
};

}  // namespace scenic_impl

#endif  // SRC_UI_SCENIC_BIN_APP_H_
