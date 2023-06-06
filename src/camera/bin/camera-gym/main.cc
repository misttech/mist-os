// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/camera3/cpp/fidl.h>
#include <fuchsia/sysmem/cpp/fidl.h>
#include <fuchsia/ui/scenic/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/fpromise/bridge.h>
#include <lib/fpromise/single_threaded_executor.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/provider.h>

#include <set>

#include "src/camera/bin/camera-gym/buffer_collage.h"
#include "src/camera/bin/camera-gym/buffer_collage_flatland.h"
#include "src/camera/bin/camera-gym/controller_receiver.h"
#include "src/camera/bin/camera-gym/frame_capture.h"
#include "src/camera/bin/camera-gym/lifecycle_impl.h"
#include "src/camera/bin/camera-gym/stream_cycler.h"
#include "src/lib/fxl/command_line.h"

using Command = fuchsia::camera::gym::Command;

constexpr zx::duration kDefaultAutoCycleInterval = zx::msec(CONFIGURATION_CYCLE_INTERVAL_MS);

namespace {
int SetupFlatland(std::optional<zx::duration> auto_cycle_interval, async::Loop* buffer_collage_loop,
                  async::Loop* cycler_loop, std::unique_ptr<sys::ComponentContext> context) {
  fuchsia::sysmem::AllocatorHandle buffer_collage_allocator;
  zx_status_t status = context->svc()->Connect(buffer_collage_allocator.NewRequest());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to request Allocator service.";
    return EXIT_FAILURE;
  }

  std::unique_ptr<simple_present::FlatlandConnection> flatland_connection =
      simple_present::FlatlandConnection::Create(context.get(), "camera-gym");

  fuchsia::ui::composition::AllocatorHandle flatland_allocator;
  status = context->svc()->Connect(flatland_allocator.NewRequest());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to request flatland allocator service.";
    return EXIT_FAILURE;
  }

  // Create the collage.
  auto collage_result = camera_flatland::BufferCollageFlatland::Create(
      std::move(flatland_connection), std::move(flatland_allocator),
      std::move(buffer_collage_allocator), [&buffer_collage_loop] { buffer_collage_loop->Quit(); });
  if (collage_result.is_error()) {
    FX_PLOGS(ERROR, collage_result.error()) << "Failed to create BufferCollageFlatland.";
    return EXIT_FAILURE;
  }
  auto collage = collage_result.take_value();

  // Connect to required services for the cycler.
  fuchsia::camera3::DeviceWatcherHandle watcher;
  status = context->svc()->Connect(watcher.NewRequest());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to request DeviceWatcher service.";
    return EXIT_FAILURE;
  }

  fuchsia::sysmem::AllocatorHandle stream_cycler_allocator;
  status = context->svc()->Connect(stream_cycler_allocator.NewRequest());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to request Allocator service.";
    return EXIT_FAILURE;
  }

  // Create the cycler and attach it to the collage.
  auto cycler_result = camera::StreamCycler::Create(
      std::move(watcher), std::move(stream_cycler_allocator), cycler_loop->dispatcher(), false);
  if (cycler_result.is_error()) {
    FX_PLOGS(ERROR, cycler_result.error()) << "Failed to create StreamCycler.";
    return EXIT_FAILURE;
  }
  auto cycler = cycler_result.take_value();
  cycler->set_auto_cycle_interval(auto_cycle_interval);

  status = cycler_loop->StartThread("StreamCycler Thread");
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status);
    return EXIT_FAILURE;
  }

  std::set<uint32_t> collection_ids;

  camera::StreamCycler::AddCollectionHandler add_collection_handler =
      [&collage, &collection_ids](fuchsia::sysmem::BufferCollectionTokenHandle token,
                                  fuchsia::sysmem::ImageFormat_2 image_format,
                                  std::string description) -> uint32_t {
    auto result = fpromise::run_single_threaded(
        collage->AddCollection(std::move(token), image_format, std::move(description)));
    if (result.is_error()) {
      FX_LOGS(FATAL) << "Failed to add collection to collage.";
      return 0;
    }
    collection_ids.insert(result.value());
    return result.value();
  };

  camera::StreamCycler::RemoveCollectionHandler remove_collection_handler =
      [&collage, &collection_ids](uint32_t id) {
        collection_ids.erase(id);
        collage->RemoveCollection(id);
      };

  // TODO(fxb/122163): Add logic to hide views if device is muted.
  camera::StreamCycler::MuteStateHandler mute_handler = [](bool muted) {};

  camera::StreamCycler::ShowBufferHandler show_buffer_handler =
      [&collage](uint32_t collection_id, uint32_t buffer_index, zx::eventpair* release_fence,
                 std::optional<fuchsia::math::RectF> subregion) {
        collage->PostShowBuffer(collection_id, buffer_index, std::move(release_fence),
                                std::move(subregion));
      };
  cycler->SetHandlers(std::move(add_collection_handler), std::move(remove_collection_handler),
                      std::move(show_buffer_handler), std::move(mute_handler));

  // Publish the view service.
  context->outgoing()->AddPublicService(collage->GetHandler());

  // Publish a handler for the Lifecycle protocol that cleanly quits the component.
  LifecycleImpl lifecycle([&buffer_collage_loop] { buffer_collage_loop->Quit(); });
  context->outgoing()->AddPublicService(lifecycle.GetHandler());

  buffer_collage_loop->Run();
  cycler_loop->Quit();
  cycler_loop->JoinThreads();
  return EXIT_SUCCESS;
}

// TODO(fxb/122163): Deprecate the gfx version of camera-gym.
int SetupGFX(bool manual_mode, std::optional<zx::duration> auto_cycle_interval,
             async::Loop* buffer_collage_loop, async::Loop* cycler_loop,
             std::unique_ptr<sys::ComponentContext> context) {
  fuchsia::sysmem::AllocatorHandle allocator;
  zx_status_t status = context->svc()->Connect(allocator.NewRequest());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to request Allocator service.";
    return EXIT_FAILURE;
  }
  fuchsia::ui::scenic::ScenicHandle scenic;
  status = context->svc()->Connect(scenic.NewRequest());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to request Scenic service.";
    return EXIT_FAILURE;
  }

  // Create the collage.
  auto collage_result =
      camera::BufferCollage::Create(std::move(scenic), std::move(allocator),
                                    [&buffer_collage_loop] { buffer_collage_loop->Quit(); });
  if (collage_result.is_error()) {
    FX_PLOGS(ERROR, collage_result.error()) << "Failed to create BufferCollage.";
    return EXIT_FAILURE;
  }
  auto collage = collage_result.take_value();

  // Support frame capture.
  if (manual_mode) {
    auto frame_capture = std::make_unique<camera::FrameCapture>();
    frame_capture->Initialize();
    collage->SetFrameCapture(std::move(frame_capture));
  }

  // Connect to required services for the cycler.
  fuchsia::camera3::DeviceWatcherHandle watcher;
  status = context->svc()->Connect(watcher.NewRequest());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to request DeviceWatcher service.";
    return EXIT_FAILURE;
  }
  allocator = nullptr;
  status = context->svc()->Connect(allocator.NewRequest());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to request Allocator service.";
    return EXIT_FAILURE;
  }

  // Create the cycler and attach it to the collage.
  auto cycler_result = camera::StreamCycler::Create(std::move(watcher), std::move(allocator),
                                                    cycler_loop->dispatcher(), manual_mode);
  if (cycler_result.is_error()) {
    FX_PLOGS(ERROR, cycler_result.error()) << "Failed to create StreamCycler.";
    return EXIT_FAILURE;
  }
  auto cycler = cycler_result.take_value();
  cycler->set_auto_cycle_interval(auto_cycle_interval);

  status = cycler_loop->StartThread("StreamCycler Thread");
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status);
    return EXIT_FAILURE;
  }

  bool device_muted = false;
  std::set<uint32_t> collection_ids;

  camera::StreamCycler::AddCollectionHandler add_collection_handler =
      [&collage, &collection_ids](fuchsia::sysmem::BufferCollectionTokenHandle token,
                                  fuchsia::sysmem::ImageFormat_2 image_format,
                                  std::string description) -> uint32_t {
    auto result = fpromise::run_single_threaded(
        collage->AddCollection(std::move(token), image_format, description));
    if (result.is_error()) {
      FX_LOGS(FATAL) << "Failed to add collection to collage.";
      return 0;
    }
    collection_ids.insert(result.value());
    return result.value();
  };

  camera::StreamCycler::RemoveCollectionHandler remove_collection_handler =
      [&collage, &collection_ids](uint32_t id) {
        collection_ids.erase(id);
        collage->RemoveCollection(id);
      };

  camera::StreamCycler::ShowBufferHandler show_buffer_handler =
      [&collage, &device_muted](uint32_t collection_id, uint32_t buffer_index,
                                zx::eventpair* release_fence,
                                std::optional<fuchsia::math::RectF> subregion) {
        collage->PostShowBuffer(collection_id, buffer_index, std::move(release_fence),
                                std::move(subregion));
        if (!device_muted) {
          // Only make the collection visible after we have shown an unmuted frame.
          collage->PostSetCollectionVisibility(collection_id, true);
        }
      };

  camera::StreamCycler::MuteStateHandler mute_handler = [&collage, &collection_ids,
                                                         &device_muted](bool muted) {
    collage->PostSetMuteIconVisibility(muted);
    if (muted) {
      // Immediately hide all collections on mute.
      for (auto id : collection_ids) {
        collage->PostSetCollectionVisibility(id, false);
      }
    }
    device_muted = muted;
  };

  cycler->SetHandlers(std::move(add_collection_handler), std::move(remove_collection_handler),
                      std::move(show_buffer_handler), std::move(mute_handler));

  std::unique_ptr<camera::ControllerReceiver> controller_receiver;
  if (manual_mode) {
    controller_receiver = std::make_unique<camera::ControllerReceiver>();
    cycler->set_controller_dispatcher(buffer_collage_loop->dispatcher());
    collage->set_controller_dispatcher(buffer_collage_loop->dispatcher());

    FX_LOGS(INFO) << "Running in manual mode.";

    // Bridge ControllerReceiver to StreamCycler.
    camera::ControllerReceiver::CommandHandler command_handler =
        [&cycler, &collage](fuchsia::camera::gym::Command command,
                            camera::ControllerReceiver::SendCommandCallback callback) {
          switch (command.Which()) {
            case Command::Tag::kSetDescription:
            case Command::Tag::kCaptureFrame:
              collage->ExecuteCommand(std::move(command), std::move(callback));
              break;
            default:
              cycler->ExecuteCommand(std::move(command), std::move(callback));
              break;
          }
        };
    controller_receiver->SetHandlers(std::move(command_handler));

    context->outgoing()->AddPublicService(controller_receiver->GetHandler());
  } else {
    FX_LOGS(INFO) << "Running in automatic mode.";
  }

  // Publish the view service.
  context->outgoing()->AddPublicService(collage->GetHandler());

  // Publish a handler for the Lifecycle protocol that cleanly quits the component.
  LifecycleImpl lifecycle([&buffer_collage_loop] { buffer_collage_loop->Quit(); });
  context->outgoing()->AddPublicService(lifecycle.GetHandler());

  buffer_collage_loop->Run();
  cycler_loop->Quit();
  cycler_loop->JoinThreads();
  return EXIT_SUCCESS;
}
}  // namespace

int main(int argc, char* argv[]) {
  auto command_line = fxl::CommandLineFromArgcArgv(argc, argv);

  // Default must match existing behavior as started from UI.
  bool manual_mode = command_line.HasOption("manual");
  bool gfx = command_line.HasOption("gfx");
  std::optional<zx::duration> auto_cycle_interval = kDefaultAutoCycleInterval;
  std::string value_string;
  if (command_line.GetOptionValue("auto-cycle-interval-ms", &value_string)) {
    auto auto_cycle_interval_ms_value = std::stoul(value_string, nullptr, 0);

    // Value of 0 means "infinite". (Meaning no config cycling at all.)
    if (auto_cycle_interval_ms_value == 0) {
      auto_cycle_interval.reset();
    } else {
      auto_cycle_interval = zx::msec(auto_cycle_interval_ms_value);
    }
  }

  fuchsia_logging::SetLogSettings({.min_log_level = CAMERA_MIN_LOG_LEVEL}, {"camera-gym"});

  async::Loop buffer_collage_loop(&kAsyncLoopConfigAttachToCurrentThread);
  async::Loop cycler_loop(&kAsyncLoopConfigNoAttachToCurrentThread);

  trace::TraceProviderWithFdio trace_provider(buffer_collage_loop.dispatcher());
  auto context = sys::ComponentContext::CreateAndServeOutgoingDirectory();
  zx_status_t status;

  if (gfx) {
    return SetupGFX(manual_mode, auto_cycle_interval, &buffer_collage_loop, &cycler_loop,
                    std::move(context));
  }
  return SetupFlatland(auto_cycle_interval, &buffer_collage_loop, &cycler_loop, std::move(context));
}
