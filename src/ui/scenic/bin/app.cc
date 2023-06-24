// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/bin/app.h"

#include <fuchsia/vulkan/loader/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>

#include <optional>

#include "rapidjson/document.h"
#include "src/lib/files/file.h"
#include "src/ui/lib/escher/vk/pipeline_builder.h"
#include "src/ui/scenic/lib/display/color_converter.h"
#include "src/ui/scenic/lib/display/display_power_manager.h"
#include "src/ui/scenic/lib/flatland/engine/engine_types.h"
#include "src/ui/scenic/lib/flatland/renderer/vk_renderer.h"
#include "src/ui/scenic/lib/gfx/api/internal_snapshot_impl.h"
#include "src/ui/scenic/lib/gfx/gfx_system.h"
#include "src/ui/scenic/lib/gfx/screenshotter.h"
#include "src/ui/scenic/lib/scheduling/frame_metrics_registry.cb.h"
#include "src/ui/scenic/lib/scheduling/windowed_frame_predictor.h"
#include "src/ui/scenic/lib/screen_capture/screen_capture.h"
#include "src/ui/scenic/lib/screen_capture/screen_capture_buffer_collection_importer.h"
#include "src/ui/scenic/lib/screenshot/screenshot_manager.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/lib/utils/metrics_impl.h"
#include "src/ui/scenic/lib/view_tree/snapshot_dump.h"
#include "src/ui/scenic/scenic_structured_config.h"

namespace {

// App installs the loader manifest FS at this path so it can use
// fsl::DeviceWatcher on it.
static const char* kDependencyPath = "/gpu-manifest-fs";

static constexpr zx::duration kShutdownTimeout = zx::sec(1);

// NOTE: If this value changes, you should also change the corresponding kCleanupDelay inside
// escher/profiling/timestamp_profiler.cc.
static constexpr zx::duration kEscherCleanupRetryInterval{1'000'000};  // 1 millisecond

enum TagType { INTEGER, BOOL };

/// ConfigValue holds a value for a given key.
struct ConfigValue {
  TagType tag;
  union {
    int int_val;
    bool bool_val;
  };
};

// Populates a ConfigValues struct by reading a config file.
scenic_impl::ConfigValues GetConfig() {
  scenic_impl::ConfigValues values;

  // Retrieve structured configuration
  auto structured_config = scenic_structured_config::Config::TakeFromStartupHandle();
  values.min_predicted_frame_duration =
      zx::msec(structured_config.frame_scheduler_min_predicted_frame_duration_in_us());
  values.i_can_haz_flatland = structured_config.i_can_haz_flatland();
  values.enable_allocator_for_flatland = structured_config.enable_allocator_for_flatland();
  values.pointer_auto_focus_on = structured_config.pointer_auto_focus();
  values.flatland_enable_display_composition =
      structured_config.flatland_enable_display_composition();

  if (structured_config.i_can_haz_display_id() < 0) {
    values.i_can_haz_display_id = std::nullopt;
  } else {
    values.i_can_haz_display_id = structured_config.i_can_haz_display_id();
  }

  if (structured_config.i_can_haz_display_mode() < 0) {
    values.i_can_haz_display_mode = std::nullopt;
  } else {
    values.i_can_haz_display_mode = structured_config.i_can_haz_display_mode();
  }

  using GetValueCallback = std::function<void(const std::string&, ConfigValue&)>;
  std::unordered_map<std::string, GetValueCallback> config{
      {
          "frame_scheduler_min_predicted_frame_duration_in_us",
          [&values](auto& key, auto& value) {
            FX_CHECK(value.tag == INTEGER) << key << " must be an integer";
            FX_CHECK(value.int_val >= 0) << key << " must be greater than 0";
            values.min_predicted_frame_duration = zx::usec(value.int_val);
          },
      },
      {
          "i_can_haz_flatland",
          [&values](auto& key, auto& value) {
            FX_CHECK(value.tag == BOOL) << key << " must be a boolean";
            values.i_can_haz_flatland = value.bool_val;
          },
      },
      {
          "enable_allocator_for_flatland",
          [&values](auto& key, auto& value) {
            FX_CHECK(value.tag == BOOL) << key << " must be a boolean";
            values.enable_allocator_for_flatland = value.bool_val;
          },
      },
      {
          "pointer_auto_focus",
          [&values](auto& key, auto& value) {
            FX_CHECK(value.tag == BOOL) << key << " must be a boolean";
            values.pointer_auto_focus_on = value.bool_val;
          },
      },
      {
          "flatland_enable_display_composition",
          [&values](auto& key, auto& value) {
            FX_CHECK(value.tag == BOOL) << key << " must be a boolean";
            values.flatland_enable_display_composition = value.bool_val;
          },
      },
      {
          "i_can_haz_display_id",
          [&values](auto& key, auto& value) {
            FX_CHECK(value.tag == INTEGER) << key << " must be an integer";
            values.i_can_haz_display_id = value.int_val;
          },
      },
      {
          "i_can_haz_display_mode",
          [&values](auto& key, auto& value) {
            FX_CHECK(value.tag == INTEGER) << key << " must be an integer";
            values.i_can_haz_display_mode = value.int_val;
          },
      },
  };

  std::string config_string;
  if (files::ReadFileToString("/config/data/scenic_config", &config_string)) {
    FX_LOGS(INFO) << "Found config file at /config/data/scenic_config";
    rapidjson::Document document;
    document.Parse(config_string);
    for (auto& [key, callback] : config) {
      if (document.HasMember(key)) {
        auto& json_value = document[key];

        ConfigValue value;
        if (json_value.IsInt()) {
          value = {INTEGER, {json_value.GetInt()}};
        } else if (json_value.IsBool()) {
          value = {BOOL, {json_value.GetBool()}};
        } else {
          FX_CHECK(false) << "Unsupported type for '" << key << "'";
        }
        callback(key, value);
      }
    }
  } else {
    FX_LOGS(INFO) << "No config file found at /config/data/scenic_config; using default values";
  }

  // Get the display_rotation value from the config file.
  if (std::string display_rotation_config;
      files::ReadFileToString("/config/data/display_rotation", &display_rotation_config)) {
    if (int display_rotation = stoi(display_rotation_config); display_rotation >= 0) {
      FX_CHECK(display_rotation < 360) << "Rotation should be less than 360 degrees.";
      values.display_rotation = display_rotation;
    } else {
      FX_LOGS(WARNING)
          << "Invalid value for display_rotation. Falling back to the default value 0.";
    }

  } else {
    FX_LOGS(INFO)
        << "No config file found at /config/data/display_rotation, using default rotation value 0";
  }

  FX_LOGS(INFO) << "Scenic min_predicted_frame_duration(us): "
                << values.min_predicted_frame_duration.to_usecs();
  FX_LOGS(INFO) << "i_can_haz_flatland: " << values.i_can_haz_flatland;
  FX_LOGS(INFO) << "enable_allocator_for_flatland: " << values.enable_allocator_for_flatland;
  FX_LOGS(INFO) << "Scenic pointer auto focus: " << values.pointer_auto_focus_on;
  FX_LOGS(INFO) << "flatland_enable_display_composition: "
                << values.flatland_enable_display_composition;
  FX_LOGS(INFO) << "Scenic i_can_haz_display_id: " << values.i_can_haz_display_id.value_or(0);
  FX_LOGS(INFO) << "Scenic i_can_haz_display_mode: " << values.i_can_haz_display_mode.value_or(0);

  return values;
}

#ifdef NDEBUG
// TODO(fxbug.dev/48596): Scenic sometimes gets stuck for consecutive 60 seconds.
// Here we set up a Watchdog polling Scenic status every 15 seconds.
constexpr uint32_t kWatchdogWarningIntervalMs = 15000u;
// On some devices, the time to start up Scenic may exceed 15 seconds.
// In that case we should only send a warning, and we should only crash
// Scenic if the main thread is blocked for longer time.
constexpr uint32_t kWatchdogTimeoutMs = 45000u;
#else   // !defined(NDEBUG)
// We set a higher warning interval and timeout length for debug builds,
// since these builds could be slower than the default release ones.
constexpr uint32_t kWatchdogWarningIntervalMs = 30000u;
constexpr uint32_t kWatchdogTimeoutMs = 90000u;
#endif  // NDEBUG

}  // namespace

namespace scenic_impl {

DisplayInfoDelegate::DisplayInfoDelegate(std::shared_ptr<display::Display> display_)
    : display_(display_) {
  FX_CHECK(display_);
}

void DisplayInfoDelegate::GetDisplayInfo(
    fuchsia::ui::scenic::Scenic::GetDisplayInfoCallback callback) {
  auto info = ::fuchsia::ui::gfx::DisplayInfo();
  info.width_in_px = display_->width_in_px();
  info.height_in_px = display_->height_in_px();

  callback(std::move(info));
}

fuchsia::math::SizeU DisplayInfoDelegate::GetDisplayDimensions() {
  return {display_->width_in_px(), display_->height_in_px()};
}

void DisplayInfoDelegate::GetDisplayOwnershipEvent(
    fuchsia::ui::scenic::Scenic::GetDisplayOwnershipEventCallback callback) {
  // These constants are defined as raw hex in the FIDL file, so we confirm here that they are the
  // same values as the expected constants in the ZX headers.
  static_assert(fuchsia::ui::scenic::displayNotOwnedSignal == ZX_USER_SIGNAL_0, "Bad constant");
  static_assert(fuchsia::ui::scenic::displayOwnedSignal == ZX_USER_SIGNAL_1, "Bad constant");

  zx::event dup;
  if (display_->ownership_event().duplicate(ZX_RIGHTS_BASIC, &dup) != ZX_OK) {
    FX_LOGS(ERROR) << "Display ownership event duplication error.";
    callback(zx::event());
  } else {
    callback(std::move(dup));
  }
}

App::App(std::unique_ptr<sys::ComponentContext> app_context, inspect::Node inspect_node,
         fpromise::promise<ui_display::DisplayCoordinatorHandles, zx_status_t> dc_handles_promise,
         fit::closure quit_callback)
    : executor_(async_get_default_dispatcher()),
      app_context_(std::move(app_context)),
      config_values_(GetConfig()),
      // TODO(fxbug.dev/40997): subsystems requiring graceful shutdown *on a loop* should register
      // themselves. It is preferable to cleanly shutdown using destructors only, if possible.
      shutdown_manager_(
          ShutdownManager::New(async_get_default_dispatcher(), std::move(quit_callback))),
      metrics_logger_(
          async_get_default_dispatcher(),
          fidl::ClientEnd<fuchsia_io::Directory>(component::OpenServiceRoot()->TakeChannel())),
      inspect_node_(std::move(inspect_node)),
      frame_scheduler_(std::make_unique<scheduling::WindowedFramePredictor>(
                           config_values_.min_predicted_frame_duration,
                           scheduling::DefaultFrameScheduler::kInitialRenderDuration,
                           scheduling::DefaultFrameScheduler::kInitialUpdateDuration),
                       inspect_node_.CreateChild("FrameScheduler"), &metrics_logger_),
      image_pipe_updater_(std::make_shared<gfx::ImagePipeUpdater>(frame_scheduler_)),
      scenic_(
          app_context_.get(), inspect_node_, frame_scheduler_,
          [weak = std::weak_ptr<ShutdownManager>(shutdown_manager_)] {
            if (auto strong = weak.lock()) {
              strong->Shutdown(kShutdownTimeout);
            }
          },
          config_values_.i_can_haz_flatland),
      uber_struct_system_(std::make_shared<flatland::UberStructSystem>()),
      link_system_(
          std::make_shared<flatland::LinkSystem>(uber_struct_system_->GetNextInstanceId())),
      flatland_presenter_(std::make_shared<flatland::FlatlandPresenterImpl>(
          async_get_default_dispatcher(), frame_scheduler_)),
      color_converter_(app_context_.get(),
                       /*set_color_conversion_values*/
                       config_values_.i_can_haz_flatland
                           ? display::SetColorConversionFunc([this](const auto& coefficients,
                                                                    const auto& preoffsets,
                                                                    const auto& postoffsets) {
                               FX_DCHECK(flatland_compositor_);
                               flatland_compositor_->SetColorConversionValues(
                                   coefficients, preoffsets, postoffsets);
                             })
                           : display::SetColorConversionFunc([this](const auto& coefficients,
                                                                    const auto& preoffsets,
                                                                    const auto& postoffsets) {
                               FX_DCHECK(engine_);
                               for (auto compositor : engine_->scene_graph()->compositors()) {
                                 if (auto swapchain = compositor->swapchain()) {
                                   const bool success = swapchain->SetDisplayColorConversion(
                                       {.preoffsets = preoffsets,
                                        .matrix = coefficients,
                                        .postoffsets = postoffsets});
                                   FX_DCHECK(success);
                                 }
                               }
                             }),
                       /*set_minimum_rgb*/
                       config_values_.i_can_haz_flatland
                           ? display::SetMinimumRgbFunc([this](const uint8_t minimum_rgb) {
                               FX_DCHECK(flatland_compositor_);
                               flatland_compositor_->SetMinimumRgb(minimum_rgb);
                             })
                           : display::SetMinimumRgbFunc([this](const uint8_t minimum_rgb) {
                               FX_DCHECK(engine_);
                               for (auto compositor : engine_->scene_graph()->compositors()) {
                                 if (auto swapchain = compositor->swapchain()) {
                                   const bool success = swapchain->SetMinimumRgb(minimum_rgb);
                                   FX_DCHECK(success);
                                 }
                               }
                             })),
      focus_manager_(inspect_node_.CreateChild("FocusManager"),
                     /*legacy_focus_listener*/
                     [this](zx_koid_t old_focus, zx_koid_t new_focus) {
                       FX_DCHECK(engine_);
                       engine_->scene_graph()->OnNewFocusedView(old_focus, new_focus);
                     }),
      geometry_provider_(),
      observer_registry_(geometry_provider_),
      scoped_observer_registry_(geometry_provider_),
      annotation_registry_(app_context_.get()),
      watchdog_("Scenic main thread", kWatchdogWarningIntervalMs, kWatchdogTimeoutMs,
                async_get_default_dispatcher()) {
  fpromise::bridge<escher::EscherUniquePtr> escher_bridge;
  fpromise::bridge<std::shared_ptr<display::Display>> display_bridge;

  auto vulkan_loader = app_context_->svc()->Connect<fuchsia::vulkan::loader::Loader>();
  fidl::InterfaceHandle<fuchsia::io::Directory> dir;
  vulkan_loader->ConnectToManifestFs(fuchsia::vulkan::loader::ConnectToManifestOptions{},
                                     dir.NewRequest().TakeChannel());

  fdio_ns_t* ns;
  FX_CHECK(fdio_ns_get_installed(&ns) == ZX_OK);
  FX_CHECK(fdio_ns_bind(ns, kDependencyPath, dir.TakeChannel().release()) == ZX_OK);

  // Publish all protocols that are ready.
  view_ref_installed_impl_.Publish(app_context_.get());
  observer_registry_.Publish(app_context_.get());
  scoped_observer_registry_.Publish(app_context_.get());
  focus_manager_.Publish(*app_context_);

  // Wait for a Vulkan ICD to become advertised before trying to launch escher.
  FX_DCHECK(!device_watcher_);
  device_watcher_ = fsl::DeviceWatcher::Create(
      kDependencyPath,
      [this, vulkan_loader = std::move(vulkan_loader),
       completer = std::move(escher_bridge.completer)](
          const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename) mutable {
        auto escher = gfx::GfxSystem::CreateEscher(app_context_.get());
        if (!escher) {
          FX_LOGS(WARNING) << "Escher creation failed.";
          // This should almost never happen, but might if the device was removed quickly after it
          // was added or if the Vulkan driver doesn't actually work on this hardware. Retry when a
          // new device is added.
          return;
        }
        completer.complete_ok(std::move(escher));
        device_watcher_.reset();
      });
  FX_DCHECK(device_watcher_);

  // Instantiate DisplayManager and schedule a task to inject the display coordinator into it, once
  // it becomes available.
  display_manager_.emplace(config_values_.i_can_haz_display_id,
                           config_values_.i_can_haz_display_mode,
                           [this, completer = std::move(display_bridge.completer)]() mutable {
                             completer.complete_ok(display_manager_->default_display_shared());
                           });
  executor_.schedule_task(dc_handles_promise.then(
      [this](fpromise::result<ui_display::DisplayCoordinatorHandles, zx_status_t>& handles) {
        display_manager_->BindDefaultDisplayCoordinator(std::move(handles.value().coordinator));
      }));

  // Schedule a task to finish initialization once all promises have been completed.
  // This closure is placed on |executor_|, which is owned by App, so it is safe to use |this|.
  {
    auto p =
        fpromise::join_promises(escher_bridge.consumer.promise(), display_bridge.consumer.promise())
            .and_then(
                [this](std::tuple<fpromise::result<escher::EscherUniquePtr>,
                                  fpromise::result<std::shared_ptr<display::Display>>>& results) {
                  InitializeServices(std::move(std::get<0>(results).value()),
                                     std::move(std::get<1>(results).value()));
                  // Should be run after all outgoing services are published.
                  app_context_->outgoing()->ServeFromStartupInfo();
                });

    executor_.schedule_task(std::move(p));
  }
}

void App::InitializeServices(escher::EscherUniquePtr escher,
                             std::shared_ptr<display::Display> display) {
  TRACE_DURATION("gfx", "App::InitializeServices");

  if (!display) {
    FX_LOGS(ERROR) << "No default display, Graphics system exiting";
    shutdown_manager_->Shutdown(kShutdownTimeout);
    return;
  }

  if (!escher || !escher->device()) {
    FX_LOGS(ERROR) << "No Vulkan on device, Graphics system exiting.";
    shutdown_manager_->Shutdown(kShutdownTimeout);
    return;
  }

  escher_ = std::move(escher);
  escher_cleanup_ = std::make_shared<utils::CleanupUntilDone>(kEscherCleanupRetryInterval,
                                                              [escher = escher_->GetWeakPtr()]() {
                                                                if (!escher) {
                                                                  // Escher is destroyed, so there
                                                                  // is no cleanup to be done.
                                                                  return true;
                                                                }
                                                                return escher->Cleanup();
                                                              });

  InitializeGraphics(display);
  InitializeInput();
  InitializeHeartbeat(*display);
}

App::~App() {
  fdio_ns_t* ns;
  FX_CHECK(fdio_ns_get_installed(&ns) == ZX_OK);
  FX_CHECK(fdio_ns_unbind(ns, kDependencyPath) == ZX_OK);
}

void App::InitializeGraphics(std::shared_ptr<display::Display> display) {
  TRACE_DURATION("gfx", "App::InitializeGraphics");
  FX_LOGS(INFO) << "App::InitializeGraphics() " << display->width_in_px() << "x"
                << display->height_in_px() << "px  " << display->width_in_mm() << "x"
                << display->height_in_mm() << "mm";

  // Replace Escher's default pipeline builder with one which will log to Cobalt upon each
  // unexpected lazy pipeline creation.  This allows us to detect when this slips through our
  // testing and occurs in the wild.  In order to detect problems ASAP during development, debug
  // builds CHECK instead of logging to Cobalt.
  {
    auto pipeline_builder = std::make_unique<escher::PipelineBuilder>(escher_->vk_device());
    pipeline_builder->set_log_pipeline_creation_callback(
        [metrics_logger = &metrics_logger_](const vk::GraphicsPipelineCreateInfo* graphics_info,
                                            const vk::ComputePipelineCreateInfo* compute_info) {
          // TODO(fxbug.dev/49972): pre-warm compute pipelines in addition to graphics pipelines.
          if (compute_info) {
            FX_LOGS(WARNING) << "Unexpected lazy creation of Vulkan compute pipeline.";
            return;
          }

#if !defined(NDEBUG)
          FX_CHECK(false)  // debug builds should crash for early detection
#else
          FX_LOGS(WARNING)  // release builds should log to Cobalt, see below.
#endif
              << "Unexpected lazy creation of Vulkan pipeline.";

          metrics_logger->LogRareEvent(
              cobalt_registry::ScenicRareEventMigratedMetricDimensionEvent::LazyPipelineCreation);
        });
    escher_->set_pipeline_builder(std::move(pipeline_builder));
  }

  auto gfx_buffer_collection_importer =
      std::make_shared<gfx::GfxBufferCollectionImporter>(escher_->GetWeakPtr());
  {
    TRACE_DURATION("gfx", "App::InitializeServices[engine]");
    engine_.emplace(escher_->GetWeakPtr(), gfx_buffer_collection_importer,
                    inspect_node_.CreateChild("Engine"));
  }

  annotation_registry_.InitializeWithGfxAnnotationManager(engine_->annotation_manager());

  auto gfx = scenic_.RegisterSystem<gfx::GfxSystem>(&engine_.value(), &sysmem_,
                                                    &display_manager_.value(), image_pipe_updater_);
  FX_DCHECK(gfx);

  scenic_.SetScreenshotDelegate(gfx.get());

  {
    singleton_display_service_.emplace(display);
    singleton_display_service_->AddPublicService(scenic_.app_context()->outgoing().get());
    display_info_delegate_.emplace(display);
    scenic_.SetDisplayInfoDelegate(&display_info_delegate_.value());
  }

  // Create the snapshotter and pass it to scenic.
  auto snapshotter =
      std::make_unique<gfx::InternalSnapshotImpl>(engine_->scene_graph(), escher_->GetWeakPtr());
  scenic_.InitializeSnapshotService(std::move(snapshotter));
  scenic_.SetRegisterViewFocuser(
      [this](zx_koid_t view_ref_koid, fidl::InterfaceRequest<fuchsia::ui::views::Focuser> focuser) {
        focus_manager_.RegisterViewFocuser(view_ref_koid, std::move(focuser));
      });
  auto flatland_renderer = std::make_shared<flatland::VkRenderer>(escher_->GetWeakPtr());
  // TODO(fxbug.dev/78186): flatland::VkRenderer hardcodes the framebuffer pixel format.
  // Eventually we won't, instead choosing one from the list of acceptable formats advertised by
  // each plugged-in display.  This will raise the issue of where to do pipeline cache warming: it
  // will be too early to do it here, since we're not yet aware of any displays nor the formats they
  // support.  It will probably be OK to warm the cache when a new display is plugged in, because
  // users don't expect plugging in a display to be completely jank-free.
  if (config_values_.i_can_haz_flatland) {
    // Warming the pipeline cache causes some non-Flatland tests to time out, so don't warm unless
    // |i_can_haz_flatland| is true.
    flatland_renderer->WarmPipelineCache();

    // TODO(fxb/122155) Support camera image in shader pre-warmup.
    // Disabling this line allows any shaders that weren't warmed up to be lazily created later.
    // flatland_renderer->set_disable_lazy_pipeline_creation(true);
  }

  // Flatland compositor must be made first; it is needed by the manager and the engine.
  {
    TRACE_DURATION("gfx", "App::InitializeServices[flatland_display_compositor]");

    flatland_compositor_ = std::make_shared<flatland::DisplayCompositor>(
        async_get_default_dispatcher(), display_manager_->default_display_coordinator(),
        flatland_renderer, utils::CreateSysmemAllocatorSyncPtr("flatland::DisplayCompositor"),
        config_values_.flatland_enable_display_composition);
  }

  // Flatland manager depends on compositor, and is required by engine.
  {
    TRACE_DURATION("gfx", "App::InitializeServices[flatland_manager]");

    std::vector<std::shared_ptr<allocation::BufferCollectionImporter>> importers{
        flatland_compositor_};

    flatland_manager_ = std::make_shared<flatland::FlatlandManager>(
        async_get_default_dispatcher(), flatland_presenter_, uber_struct_system_, link_system_,
        display, std::move(importers),
        /*register_view_focuser*/
        [this](fidl::InterfaceRequest<fuchsia::ui::views::Focuser> focuser,
               zx_koid_t view_ref_koid) {
          focus_manager_.RegisterViewFocuser(view_ref_koid, std::move(focuser));
        },
        /*register_view_ref_focused*/
        [this](fidl::InterfaceRequest<fuchsia::ui::views::ViewRefFocused> vrf,
               zx_koid_t view_ref_koid) {
          focus_manager_.RegisterViewRefFocused(view_ref_koid, std::move(vrf));
        },
        /*register_touch_source*/
        [this](fidl::InterfaceRequest<fuchsia::ui::pointer::TouchSource> touch_source,
               zx_koid_t view_ref_koid) {
          input_->RegisterTouchSource(std::move(touch_source), view_ref_koid);
        },
        /*register_mouse_source*/
        [this](fidl::InterfaceRequest<fuchsia::ui::pointer::MouseSource> mouse_source,
               zx_koid_t view_ref_koid) {
          input_->RegisterMouseSource(std::move(mouse_source), view_ref_koid);
        });

    // TODO(fxbug.dev/67206): these should be moved into FlatlandManager.
    {
      fit::function<void(fidl::InterfaceRequest<fuchsia::ui::composition::Flatland>)> handler =
          fit::bind_member(flatland_manager_.get(), &flatland::FlatlandManager::CreateFlatland);
      FX_CHECK(app_context_->outgoing()->AddPublicService(std::move(handler)) == ZX_OK);
    }
    {
      fit::function<void(fidl::InterfaceRequest<fuchsia::ui::composition::FlatlandDisplay>)>
          handler = fit::bind_member(flatland_manager_.get(),
                                     &flatland::FlatlandManager::CreateFlatlandDisplay);
      FX_CHECK(app_context_->outgoing()->AddPublicService(std::move(handler)) == ZX_OK);
    }
  }

  const auto screen_capture_buffer_collection_importer =
      std::make_shared<screen_capture::ScreenCaptureBufferCollectionImporter>(
          utils::CreateSysmemAllocatorSyncPtr("ScreenCaptureBufferCollectionImporter"),
          flatland_renderer);

  // Allocator service needs Flatland DisplayCompositor to act as a BufferCollectionImporter.
  {
    std::vector<std::shared_ptr<allocation::BufferCollectionImporter>> screen_capture_importers;
    screen_capture_importers.push_back(screen_capture_buffer_collection_importer);

    std::vector<std::shared_ptr<allocation::BufferCollectionImporter>> default_importers;
    if (!config_values_.i_can_haz_flatland)
      default_importers.push_back(gfx_buffer_collection_importer);
    if (config_values_.enable_allocator_for_flatland && flatland_compositor_)
      default_importers.push_back(flatland_compositor_);

    allocator_ = std::make_shared<allocation::Allocator>(
        app_context_.get(), default_importers, screen_capture_importers,
        utils::CreateSysmemAllocatorSyncPtr("ScenicAllocator"));
  }

  // Flatland engine requires FlatlandManager and DisplayCompositor to be constructed first.
  {
    TRACE_DURATION("gfx", "App::InitializeServices[flatland_engine]");

    flatland_engine_ = std::make_shared<flatland::Engine>(
        flatland_compositor_, flatland_presenter_, uber_struct_system_, link_system_,
        inspect_node_.CreateChild("FlatlandEngine"), [this] {
          FX_DCHECK(flatland_manager_);
          const auto display = flatland_manager_->GetPrimaryFlatlandDisplayForRendering();
          return display ? std::optional<flatland::TransformHandle>(display->root_transform())
                         : std::nullopt;
        });
  }

  // Make ScreenCaptureManager.
  {
    TRACE_DURATION("gfx", "App::InitializeServices[screen_capture_manager]");

    std::vector<std::shared_ptr<allocation::BufferCollectionImporter>> screen_capture_importers;
    screen_capture_importers.push_back(screen_capture_buffer_collection_importer);

    // Capture flatland_manager since the primary display may not have been initialized yet.
    screen_capture_manager_.emplace(flatland_engine_, flatland_renderer, flatland_manager_,
                                    std::move(screen_capture_importers));

    fit::function<void(fidl::InterfaceRequest<fuchsia::ui::composition::ScreenCapture>)> handler =
        fit::bind_member(&screen_capture_manager_.value(),
                         &screen_capture::ScreenCaptureManager::CreateClient);
    FX_CHECK(app_context_->outgoing()->AddPublicService(std::move(handler)) == ZX_OK);
  }

  // Make ScreenCapture2Manager.
  {
    TRACE_DURATION("gfx", "App::InitializeServices[screen_capture2_manager]");

    // Capture flatland_manager since the primary display may not have been initialized yet.
    screen_capture2_manager_.emplace(
        flatland_renderer, screen_capture_buffer_collection_importer, [this]() {
          FX_DCHECK(flatland_manager_);
          FX_DCHECK(flatland_engine_);

          auto display = flatland_manager_->GetPrimaryFlatlandDisplayForRendering();
          FX_DCHECK(display);

          return flatland_engine_->GetRenderables(*display);
        });

    fit::function<void(fidl::InterfaceRequest<fuchsia::ui::composition::internal::ScreenCapture>)>
        handler = fit::bind_member(&screen_capture2_manager_.value(),
                                   &screen_capture2::ScreenCapture2Manager::CreateClient);
    FX_CHECK(app_context_->outgoing()->AddPublicService(std::move(handler)) == ZX_OK);
  }

  // Make ScreenshotManager for the client-friendly screenshot protocol.
  {
    TRACE_DURATION("gfx", "App::InitializeServices[screenshot_manager]");

    std::vector<std::shared_ptr<allocation::BufferCollectionImporter>> screen_capture_importers;
    screen_capture_importers.push_back(screen_capture_buffer_collection_importer);

    // Capture flatland_manager since the primary display may not have been initialized yet.
    screenshot_manager_.emplace(
        config_values_.i_can_haz_flatland, allocator_, flatland_renderer,
        [this]() {
          auto display = flatland_manager_->GetPrimaryFlatlandDisplayForRendering();
          return flatland_engine_->GetRenderables(*display);
        },
        [this](fuchsia::ui::scenic::Scenic::TakeScreenshotCallback callback) {
          gfx::Screenshotter::TakeScreenshot(&engine_.value(), std::move(callback),
                                             escher_cleanup_);
        },
        std::move(screen_capture_importers), display_info_delegate_->GetDisplayDimensions(),
        config_values_.display_rotation);

    fit::function<void(fidl::InterfaceRequest<fuchsia::ui::composition::Screenshot>)> handler =
        fit::bind_member(&screenshot_manager_.value(),
                         &screenshot::ScreenshotManager::CreateBinding);
    FX_CHECK(app_context_->outgoing()->AddPublicService(std::move(handler)) == ZX_OK);
  }

  {
    TRACE_DURATION("gfx", "App::InitializeServices[display_power]");
    display_power_manager_.emplace(display_manager_.value());
    FX_CHECK(app_context_->outgoing()->AddPublicService(display_power_manager_->GetHandler()) ==
             ZX_OK);
  }
}

void App::InitializeInput() {
  TRACE_DURATION("gfx", "App::InitializeInput");
  input_.emplace(app_context_.get(), inspect_node_,
                 /*request_focus*/
                 [this, use_auto_focus = config_values_.pointer_auto_focus_on](zx_koid_t koid) {
                   if (!use_auto_focus)
                     return;

                   const auto& focus_chain = focus_manager_.focus_chain();
                   if (!focus_chain.empty()) {
                     const zx_koid_t requestor = focus_chain[0];
                     const zx_koid_t request = koid != ZX_KOID_INVALID ? koid : requestor;
                     focus_manager_.RequestFocus(requestor, request);
                   }
                 });
  scenic_.SetRegisterTouchSource(
      [this](fidl::InterfaceRequest<fuchsia::ui::pointer::TouchSource> touch_source,
             zx_koid_t vrf) { input_->RegisterTouchSource(std::move(touch_source), vrf); });
  scenic_.SetRegisterMouseSource(
      [this](fidl::InterfaceRequest<fuchsia::ui::pointer::MouseSource> mouse_source,
             zx_koid_t vrf) { input_->RegisterMouseSource(std::move(mouse_source), vrf); });

  scenic_.SetViewRefFocusedRegisterFunction(
      [this](zx_koid_t koid, fidl::InterfaceRequest<fuchsia::ui::views::ViewRefFocused> vrf) {
        focus_manager_.RegisterViewRefFocused(koid, std::move(vrf));
      });
}

void App::InitializeHeartbeat(display::Display& display) {
  TRACE_DURATION("gfx", "App::InitializeHeartbeat");
  {  // Initialize ViewTreeSnapshotter

    // These callbacks are be called once per frame (at the end of OnCpuWorkDone()) and the results
    // used to build the ViewTreeSnapshot.
    // We create one per compositor.
    std::vector<view_tree::SubtreeSnapshotGenerator> subtrees_generator_callbacks;
    subtrees_generator_callbacks.emplace_back([this] {
      if (auto display = flatland_manager_->GetPrimaryFlatlandDisplayForRendering()) {
        return flatland_engine_->GenerateViewTreeSnapshot(display->root_transform());
      } else {
        return view_tree::SubtreeSnapshot{};  // Empty snapshot.
      }
    });
    // The i_can_haz_flatland flag is about eager-forcing of Flatland.
    // If true, then we KNOW that GFX should *not* run. Workstation is true.
    // if false, then either system could legitimately run. This flag is false for tests and
    // GFX-based products.
    if (!config_values_.i_can_haz_flatland) {
      subtrees_generator_callbacks.emplace_back(
          [this] { return engine_->scene_graph()->view_tree().Snapshot(); });
    }

    // All subscriber callbacks get called with the new snapshot every time one is generated (once
    // per frame).
    std::vector<view_tree::ViewTreeSnapshotter::Subscriber> subscribers;
    subscribers.push_back(
        {.on_new_view_tree =
             [this](auto snapshot) { input_->OnNewViewTreeSnapshot(std::move(snapshot)); },
         .dispatcher = async_get_default_dispatcher()});

    subscribers.push_back(
        {.on_new_view_tree =
             [this](auto snapshot) { focus_manager_.OnNewViewTreeSnapshot(std::move(snapshot)); },
         .dispatcher = async_get_default_dispatcher()});

    subscribers.push_back({.on_new_view_tree =
                               [this](auto snapshot) {
                                 view_ref_installed_impl_.OnNewViewTreeSnapshot(
                                     std::move(snapshot));
                               },
                           .dispatcher = async_get_default_dispatcher()});

    subscribers.push_back({.on_new_view_tree =
                               [this](auto snapshot) {
                                 geometry_provider_.OnNewViewTreeSnapshot(std::move(snapshot));
                               },
                           .dispatcher = async_get_default_dispatcher()});

    if (enable_snapshot_dump_) {
      subscribers.push_back({.on_new_view_tree =
                                 [](auto snapshot) {
                                   view_tree::SnapshotDump::OnNewViewTreeSnapshot(
                                       std::move(snapshot));
                                 },
                             .dispatcher = async_get_default_dispatcher()});
    }

    view_tree_snapshotter_.emplace(std::move(subtrees_generator_callbacks), std::move(subscribers));
  }

  // Set up what to do each time a FrameScheduler event fires.
  frame_scheduler_.Initialize(
      display.vsync_timing(),
      /*update_sessions*/
      [this](auto& sessions_to_update, auto trace_id, auto fences_from_previous_presents) {
        TRACE_DURATION("gfx", "App update_sessions");
        if (config_values_.i_can_haz_flatland) {
          // Flatland doesn't pass release fences into the FrameScheduler. Instead, they are stored
          // in the FlatlandPresenter and pulled out by the flatland::Engine during rendering.
          FX_CHECK(fences_from_previous_presents.empty())
              << "Flatland fences should not be handled by FrameScheduler.";
        } else {
          engine_->SignalFencesWhenPreviousRendersAreDone(std::move(fences_from_previous_presents));
        }

        const scheduling::SessionsWithFailedUpdates failed_sessions =
            scenic_.UpdateSessions(sessions_to_update, trace_id);
        image_pipe_updater_->UpdateSessions(sessions_to_update);
        flatland_manager_->UpdateInstances(sessions_to_update);
        flatland_presenter_->AccumulateReleaseFences(sessions_to_update);
        return failed_sessions;
      },
      /*on_cpu_work_done*/
      [this] {
        TRACE_DURATION("gfx", "App on_cpu_work_done");
        flatland_manager_->SendHintsToStartRendering();
        screen_capture2_manager_->RenderPendingScreenCaptures();
        view_tree_snapshotter_->UpdateSnapshot();
        // Always defer the first cleanup attempt, because the first try is almost guaranteed to
        // fail, and checking the status of a `VkFence` is fairly expensive.
        escher_cleanup_->Cleanup(/*ok_to_run_immediately=*/false);
      },
      /*on_frame_presented*/
      [this](auto latched_times, auto present_times) {
        TRACE_DURATION("gfx", "App on_frame_presented");
        scenic_.OnFramePresented(latched_times, present_times);
        image_pipe_updater_->OnFramePresented(latched_times, present_times);
        flatland_manager_->OnFramePresented(latched_times, present_times);
      },
      /*render_scheduled_frame*/
      [this](auto frame_number, auto presentation_time, auto callback) {
        TRACE_DURATION("gfx", "App render_scheduled_frame");
        FX_CHECK(flatland_frame_count_ + gfx_frame_count_ == frame_number - 1);
        if (auto display = flatland_manager_->GetPrimaryFlatlandDisplayForRendering()) {
          flatland_engine_->RenderScheduledFrame(++flatland_frame_count_, presentation_time,
                                                 *display, std::move(callback));
        } else {
          // Render the good ol' Gfx Engine way.
          engine_->RenderScheduledFrame(++gfx_frame_count_, presentation_time, std::move(callback));
        }
      });
}

}  // namespace scenic_impl
