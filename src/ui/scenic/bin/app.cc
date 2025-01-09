// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/bin/app.h"

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fuchsia/vulkan/loader/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/syslog/cpp/macros.h>

#include <cstdint>
#include <memory>
#include <optional>

#include "rapidjson/document.h"
#include "src/graphics/display/lib/coordinator-getter/client.h"
#include "src/lib/files/file.h"
#include "src/lib/fxl/functional/cancelable_callback.h"
#include "src/ui/lib/escher/vk/pipeline_builder.h"
#include "src/ui/scenic/lib/display/color_converter.h"
#include "src/ui/scenic/lib/display/display_manager.h"
#include "src/ui/scenic/lib/display/display_power_manager.h"
#include "src/ui/scenic/lib/flatland/engine/engine.h"
#include "src/ui/scenic/lib/flatland/engine/engine_types.h"
#include "src/ui/scenic/lib/flatland/renderer/cpu_renderer.h"
#include "src/ui/scenic/lib/flatland/renderer/null_renderer.h"
#include "src/ui/scenic/lib/flatland/renderer/vk_renderer.h"
#include "src/ui/scenic/lib/scheduling/frame_metrics_registry.cb.h"
#include "src/ui/scenic/lib/scheduling/windowed_frame_predictor.h"
#include "src/ui/scenic/lib/screen_capture/screen_capture.h"
#include "src/ui/scenic/lib/screen_capture/screen_capture_buffer_collection_importer.h"
#include "src/ui/scenic/lib/screenshot/screenshot_manager.h"
#include "src/ui/scenic/lib/utils/escher_provider.h"
#include "src/ui/scenic/lib/utils/helpers.h"
#include "src/ui/scenic/lib/utils/metrics_impl.h"
#include "src/ui/scenic/lib/utils/range_inclusive.h"
#include "src/ui/scenic/lib/view_tree/snapshot_dump.h"
#include "src/ui/scenic/scenic_structured_config.h"

namespace {

using scenic_impl::RendererType;

// App installs the loader manifest FS at this path so it can use
// fsl::DeviceWatcher on it.
static const char* kDependencyPath = "/gpu-manifest-fs";

static constexpr zx::duration kShutdownTimeout = zx::sec(1);

// NOTE: If this value changes, you should also change the corresponding kCleanupDelay inside
// escher/profiling/timestamp_profiler.cc.
static constexpr zx::duration kEscherCleanupRetryInterval{1'000'000};  // 1 millisecond

std::optional<fuchsia_hardware_display_types::DisplayId> GetDisplayId(
    const scenic_structured_config::Config& values) {
  if (values.i_can_haz_display_id() < 0) {
    return std::nullopt;
  }
  return std::make_optional<fuchsia_hardware_display_types::DisplayId>(
      {{.value = static_cast<uint64_t>(values.i_can_haz_display_id())}});
}

std::optional<uint64_t> GetDisplayMode(const scenic_structured_config::Config& values) {
  if (values.i_can_haz_display_mode() < 0) {
    return std::nullopt;
  }
  return values.i_can_haz_display_mode();
}

utils::RangeInclusive<int> CreateRangeFromStructuredConfigValues(int left, int right) {
  if (left >= 0 && right >= 0) {
    ZX_DEBUG_ASSERT(left <= right);
    return utils::RangeInclusive<int>(left, right);
  }
  if (left >= 0) {
    return utils::RangeInclusive<int>(left, utils::PositiveInfinity{});
  }
  if (right >= 0) {
    return utils::RangeInclusive<int>(utils::NegativeInfinity{}, right);
  }
  return utils::RangeInclusive<int>();
}

scenic_impl::display::DisplayModeConstraints GetDisplayModeConstraints(
    const scenic_structured_config::Config& values) {
  return {
      .width_px_range =
          CreateRangeFromStructuredConfigValues(values.min_display_horizontal_resolution_px(),
                                                values.max_display_horizontal_resolution_px()),
      .height_px_range = CreateRangeFromStructuredConfigValues(
          values.min_display_vertical_resolution_px(), values.max_display_vertical_resolution_px()),
      .refresh_rate_millihertz_range =
          CreateRangeFromStructuredConfigValues(values.min_display_refresh_rate_millihertz(),
                                                values.max_display_refresh_rate_millihertz()),
  };
}

std::string ToString(RendererType type) {
  switch (type) {
    case RendererType::CPU_RENDERER:
      return "cpu";
    case RendererType::NULL_RENDERER:
      return "null";
    case RendererType::VULKAN:
      return "vulkan";
  }
}

RendererType GetRendererType(const scenic_structured_config::Config& values) {
  if (ToString(RendererType::CPU_RENDERER).compare(values.renderer()) == 0)
    return RendererType::CPU_RENDERER;
  if (ToString(RendererType::NULL_RENDERER).compare(values.renderer()) == 0)
    return RendererType::NULL_RENDERER;
  if (ToString(RendererType::VULKAN).compare(values.renderer()) == 0)
    return RendererType::VULKAN;
  FX_LOGS(WARNING) << "Unknown renderer type: " << values.renderer() << ". Falling back to vulkan";
  return RendererType::VULKAN;
}

uint64_t GetDisplayRotation(scenic_structured_config::Config values) {
  uint64_t rotation = values.display_rotation();
  if (rotation >= 0) {
    FX_CHECK(rotation < 360) << "Rotation should be less than 360 degrees.";
    return rotation;
  }
  FX_LOGS(WARNING) << "Invalid value for display_rotation. Falling back to the default value 0.";
  return 0;
}

// Gets Scenic's structured config values and logs them.
scenic_structured_config::Config GetConfig() {
  // Retrieve structured configuration
  auto values = scenic_structured_config::Config::TakeFromStartupHandle();

  FX_LOGS(INFO) << "Scenic renderer: " << ToString(GetRendererType(values))
                << " min_predicted_frame_duration(us): "
                << values.frame_scheduler_min_predicted_frame_duration_in_us()
                << " pointer auto focus: " << values.pointer_auto_focus()
                << " display_composition: " << values.display_composition()
                << " i_can_haz_display_id: "
                << GetDisplayId(values)
                       .value_or(fuchsia_hardware_display_types::DisplayId{
                           {.value = fuchsia_hardware_display_types::kInvalidDispId}})
                       .value()
                << " i_can_haz_display_mode: " << GetDisplayMode(values).value_or(0)
                << " display_rotation: " << GetDisplayRotation(values)
                << " visual_debugging_level: " << static_cast<int>(values.visual_debugging_level());

  return values;
}

// Interval at which we log that Scenic is waiting for Vulkan or display.
static constexpr zx::duration kWaitWarningInterval = zx::sec(5);

void PostDelayedTaskUntilCancelled(fit::closure cb, zx::duration delay, bool first_run = true) {
  if (!cb)
    return;
  if (!first_run)
    cb();
  async::PostDelayedTask(
      async_get_default_dispatcher(),
      [cb = std::move(cb), delay]() mutable {
        PostDelayedTaskUntilCancelled(std::move(cb), delay, false);
      },
      delay);
}

}  // namespace

namespace scenic_impl {

DisplayInfoDelegate::DisplayInfoDelegate(std::shared_ptr<display::Display> display_)
    : display_(display_) {
  FX_CHECK(display_);
}

fuchsia::math::SizeU DisplayInfoDelegate::GetDisplayDimensions() {
  return {display_->width_in_px(), display_->height_in_px()};
}

App::App(std::unique_ptr<sys::ComponentContext> app_context, inspect::Node inspect_node,
         fpromise::promise<::display::CoordinatorClientChannels, zx_status_t> dc_handles_promise,
         fit::closure quit_callback)
    : executor_(async_get_default_dispatcher()),
      app_context_(std::move(app_context)),
      config_values_(GetConfig()),
      // TODO(https://fxbug.dev/42117030): subsystems requiring graceful shutdown *on a loop* should
      // register themselves. It is preferable to cleanly shutdown using destructors only, if
      // possible.
      shutdown_manager_(
          ShutdownManager::New(async_get_default_dispatcher(), std::move(quit_callback))),
      metrics_logger_(
          async_get_default_dispatcher(),
          fidl::ClientEnd<fuchsia_io::Directory>(component::OpenServiceRoot()->TakeChannel())),
      inspect_node_(std::move(inspect_node)),
      frame_scheduler_(
          std::make_unique<scheduling::WindowedFramePredictor>(
              zx::usec(config_values_.frame_scheduler_min_predicted_frame_duration_in_us()),
              scheduling::DefaultFrameScheduler::kInitialRenderDuration,
              scheduling::DefaultFrameScheduler::kInitialUpdateDuration),
          inspect_node_.CreateChild("FrameScheduler"), &metrics_logger_),
      renderer_type_(GetRendererType(config_values_)),
      uber_struct_system_(std::make_shared<flatland::UberStructSystem>()),
      link_system_(
          std::make_shared<flatland::LinkSystem>(uber_struct_system_->GetNextInstanceId())),
      flatland_presenter_(std::make_shared<flatland::FlatlandPresenterImpl>(
          async_get_default_dispatcher(), frame_scheduler_)),
      color_converter_(
          app_context_.get(),
          /*set_color_conversion_values*/
          display::SetColorConversionFunc([this](const auto& coefficients, const auto& preoffsets,
                                                 const auto& postoffsets) {
            FX_DCHECK(flatland_compositor_);
            flatland_compositor_->SetColorConversionValues(coefficients, preoffsets, postoffsets);
          }),
          /*set_minimum_rgb*/
          display::SetMinimumRgbFunc([this](const uint8_t minimum_rgb) {
            FX_DCHECK(flatland_compositor_);
            return flatland_compositor_->SetMinimumRgb(minimum_rgb);
          })),
      geometry_provider_(),
      observer_registry_(geometry_provider_),
      scoped_observer_registry_(geometry_provider_) {
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

  auto vulkan_wait_log = std::make_unique<fxl::CancelableClosure>(
      [] { FX_LOGS(WARNING) << "SCENIC IS WAITING FOR VULKAN TO BE AVAILABLE..."; });
  PostDelayedTaskUntilCancelled(vulkan_wait_log->callback(), kWaitWarningInterval);

  if (renderer_type_ == RendererType::VULKAN) {
    // Wait for a Vulkan ICD to become advertised before trying to launch escher.
    FX_DCHECK(!device_watcher_);
    device_watcher_ = fsl::DeviceWatcher::Create(
        kDependencyPath, [this, vulkan_loader = std::move(vulkan_loader),
                          completer = std::move(escher_bridge.completer),
                          vulkan_wait_log = std::move(vulkan_wait_log)](
                             const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                             const std::string& filename) mutable {
          auto escher = utils::CreateEscher(app_context_.get());
          if (!escher) {
            FX_LOGS(WARNING) << "Escher creation failed.";
            // This should almost never happen, but might if the device was removed quickly after it
            // was added or if the Vulkan driver doesn't actually work on this hardware. Retry when
            // a new device is added.
            return;
          }
          completer.complete_ok(std::move(escher));
          device_watcher_.reset();
        });
    FX_DCHECK(device_watcher_);
  } else {
    // Immediately complete promise if we aren't using vulkan renderer.
    escher_bridge.completer.complete_ok(nullptr);
  }

  auto display_wait_log = std::make_unique<fxl::CancelableClosure>(
      [] { FX_LOGS(WARNING) << "SCENIC IS WAITING FOR DISPLAY TO BE AVAILABLE..."; });
  PostDelayedTaskUntilCancelled(display_wait_log->callback(), kWaitWarningInterval);

  // Instantiate DisplayManager and schedule a task to inject the display coordinator into it, once
  // it becomes available.
  display_manager_.emplace(GetDisplayId(config_values_), GetDisplayMode(config_values_),
                           GetDisplayModeConstraints(config_values_),
                           [this, completer = std::move(display_bridge.completer),
                            display_wait_log = std::move(display_wait_log)]() mutable {
                             completer.complete_ok(display_manager_->default_display_shared());
                           });
  executor_.schedule_task(dc_handles_promise.then(
      [this](fpromise::result<::display::CoordinatorClientChannels, zx_status_t>& client_channels) {
        FX_CHECK(client_channels.is_ok()) << "Failed to get display coordinator:"
                                          << zx_status_get_string(client_channels.error());
        auto [coordinator_client, listener_server] = std::move(client_channels.value());
        display_manager_->BindDefaultDisplayCoordinator(std::move(coordinator_client),
                                                        std::move(listener_server));
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

  if (renderer_type_ == RendererType::VULKAN) {
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
  }

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
  if (renderer_type_ == RendererType::VULKAN) {
    auto pipeline_builder = std::make_unique<escher::PipelineBuilder>(escher_->vk_device());
    pipeline_builder->set_log_pipeline_creation_callback(
        [metrics_logger = &metrics_logger_](const vk::GraphicsPipelineCreateInfo* graphics_info,
                                            const vk::ComputePipelineCreateInfo* compute_info) {
          // TODO(https://fxbug.dev/42126999): pre-warm compute pipelines in addition to graphics
          // pipelines.
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

  {
    singleton_display_service_.emplace(display);
    singleton_display_service_->AddPublicService(app_context_->outgoing().get());
    display_info_delegate_.emplace(display);
  }

  std::shared_ptr<flatland::Renderer> flatland_renderer;
  switch (renderer_type_) {
    case RendererType::CPU_RENDERER:
      flatland_renderer = std::make_shared<flatland::CpuRenderer>();
      break;
    case RendererType::NULL_RENDERER:
      flatland_renderer = std::make_shared<flatland::NullRenderer>();
      break;
    case RendererType::VULKAN:
      flatland_renderer = std::make_shared<flatland::VkRenderer>(escher_->GetWeakPtr());
      break;
  }
  // TODO(https://fxbug.dev/42158284): flatland::VkRenderer hardcodes the framebuffer pixel format.
  // Eventually we won't, instead choosing one from the list of acceptable formats advertised by
  // each plugged-in display.  This will raise the issue of where to do pipeline cache warming: it
  // will be too early to do it here, since we're not yet aware of any displays nor the formats they
  // support.  It will probably be OK to warm the cache when a new display is plugged in, because
  // users don't expect plugging in a display to be completely jank-free.

  flatland_renderer->WarmPipelineCache();

  // TODO(https://fxbug.dev/42073146) Support camera image in shader pre-warmup.
  // Disabling this line allows any shaders that weren't warmed up to be lazily created later.
  // flatland_renderer->set_disable_lazy_pipeline_creation(true);

  // Flatland compositor must be made first; it is needed by the manager and the engine.
  {
    TRACE_DURATION("gfx", "App::InitializeServices[flatland_display_compositor]");

    flatland_compositor_ = std::make_shared<flatland::DisplayCompositor>(
        async_get_default_dispatcher(), display_manager_->default_display_coordinator(),
        flatland_renderer, utils::CreateSysmemAllocatorSyncPtr("flatland::DisplayCompositor"),
        config_values_.display_composition(), /*max_display_layers=*/1,
        config_values_.visual_debugging_level());
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

    // TODO(https://fxbug.dev/42146099): these should be moved into FlatlandManager.
    {
      // Note: can't use `fit::bind_member()` here, because `CreateFlatland()` returns non-void.
      fit::function<void(fidl::InterfaceRequest<fuchsia::ui::composition::Flatland>)> handler =
          [flatland_manager = flatland_manager_.get()](
              fidl::InterfaceRequest<fuchsia::ui::composition::Flatland> request) {
            flatland_manager->CreateFlatland(std::move(request));
          };
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
          if (!display) {
            FX_LOGS(WARNING)
                << "No FlatlandDisplay attached at root. Returning an empty screenshot.";
            return flatland::Renderables();
          }

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
        allocator_, flatland_renderer,
        [this]() {
          FX_DCHECK(flatland_manager_);
          FX_DCHECK(flatland_engine_);

          auto display = flatland_manager_->GetPrimaryFlatlandDisplayForRendering();
          if (!display) {
            FX_LOGS(WARNING)
                << "No FlatlandDisplay attached at root. Returning an empty screenshot.";
            return flatland::Renderables();
          }

          return flatland_engine_->GetRenderables(*display);
        },
        std::move(screen_capture_importers), display_info_delegate_->GetDisplayDimensions(),
        GetDisplayRotation(config_values_));

    fit::function<void(fidl::InterfaceRequest<fuchsia::ui::composition::Screenshot>)> handler =
        fit::bind_member(&screenshot_manager_.value(),
                         &screenshot::ScreenshotManager::CreateBinding);
    FX_CHECK(app_context_->outgoing()->AddPublicService(std::move(handler)) == ZX_OK);
  }

  {
    TRACE_DURATION("gfx", "App::InitializeServices[display_power]");
    display_power_manager_.emplace(display_manager_.value(), inspect_node_);
    FX_CHECK(app_context_->outgoing()->AddProtocol<fuchsia_ui_display_singleton::DisplayPower>(
                 display_power_manager_->GetHandler()) == ZX_OK);
    display_power_manager_deprecated_.emplace(display_manager_.value(), inspect_node_);
    FX_CHECK(app_context_->outgoing()->AddPublicService(
                 display_power_manager_deprecated_->GetHandler()) == ZX_OK);
  }
}

void App::InitializeInput() {
  TRACE_DURATION("gfx", "App::InitializeInput");
  input_.emplace(app_context_.get(), inspect_node_,
                 /*request_focus*/
                 [this, use_auto_focus = config_values_.pointer_auto_focus()](zx_koid_t koid) {
                   if (!use_auto_focus)
                     return;

                   const auto& focus_chain = focus_manager_.focus_chain();
                   if (!focus_chain.empty()) {
                     const zx_koid_t requestor = focus_chain[0];
                     const zx_koid_t request = koid != ZX_KOID_INVALID ? koid : requestor;
                     focus_manager_.RequestFocus(requestor, request);
                   }
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

        // Flatland doesn't pass release fences into the FrameScheduler. Instead, they are stored
        // in the FlatlandPresenter and pulled out by the flatland::Engine during rendering.
        FX_CHECK(fences_from_previous_presents.empty())
            << "Flatland fences should not be handled by FrameScheduler.";

        flatland_manager_->UpdateInstances(sessions_to_update);
        flatland_presenter_->AccumulateReleaseFences(sessions_to_update);
      },
      /*on_cpu_work_done*/
      [this] {
        TRACE_DURATION("gfx", "App on_cpu_work_done");
        flatland_manager_->SendHintsToStartRendering();
        screen_capture2_manager_->RenderPendingScreenCaptures();
        view_tree_snapshotter_->UpdateSnapshot();
        // Always defer the first cleanup attempt, because the first try is almost guaranteed to
        // fail, and checking the status of a `VkFence` is fairly expensive.
        if (escher_cleanup_)
          escher_cleanup_->Cleanup(/*ok_to_run_immediately=*/false);
      },
      /*on_frame_presented*/
      [this](auto latched_times, auto present_times) {
        TRACE_DURATION("gfx", "App on_frame_presented");
        flatland_manager_->OnFramePresented(latched_times, present_times);
      },
      /*render_scheduled_frame*/
      [this](auto frame_number, auto presentation_time, auto callback) {
        TRACE_DURATION("gfx", "App render_scheduled_frame");
        FX_CHECK(flatland_frame_count_ + skipped_frame_count_ == frame_number - 1);
        if (auto display = flatland_manager_->GetPrimaryFlatlandDisplayForRendering()) {
          flatland_engine_->RenderScheduledFrame(frame_number, presentation_time, *display,
                                                 std::move(callback));
          ++flatland_frame_count_;
        } else {
          FX_LOGS(INFO) << "No FlatlandDisplay; skipping render scheduled frame.";
          skipped_frame_count_++;
          flatland_engine_->SkipRender(std::move(callback));
        }
      });
}

}  // namespace scenic_impl
