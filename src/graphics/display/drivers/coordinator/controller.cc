// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/controller.h"

#include <fuchsia/hardware/audiotypes/c/banjo.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/async/cpp/task.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fit/function.h>
#include <lib/stdcompat/span.h>
#include <lib/trace/event.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zx/channel.h>
#include <lib/zx/clock.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/threads.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <algorithm>
#include <cinttypes>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <optional>
#include <utility>
#include <vector>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/auto_lock.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/coordinator/client.h"
#include "src/graphics/display/drivers/coordinator/display-info.h"
#include "src/graphics/display/drivers/coordinator/image.h"
#include "src/graphics/display/drivers/coordinator/layer.h"
#include "src/graphics/display/drivers/coordinator/migration-util.h"
#include "src/graphics/display/drivers/coordinator/post-display-task.h"
#include "src/graphics/display/drivers/coordinator/vsync-monitor.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-capture-image-id.h"
#include "src/graphics/display/lib/edid/edid.h"
#include "src/graphics/display/lib/edid/timings.h"

namespace fidl_display = fuchsia_hardware_display;

namespace display {

void Controller::PopulateDisplayTimings(const fbl::RefPtr<DisplayInfo>& info) {
  if (!info->edid.has_value()) {
    return;
  }

  // Go through all the display mode timings and record whether or not
  // a basic layer configuration is acceptable.
  layer_t test_layers[] = {
      {
          .type = LAYER_TYPE_PRIMARY,
      },
  };
  display_config_t test_configs[] = {
      {
          .display_id = ToBanjoDisplayId(info->id),
          .layer_list = test_layers,
          .layer_count = 1,
      },
  };

  for (auto edid_timing = edid::timing_iterator(&info->edid->base); edid_timing.is_valid();
       ++edid_timing) {
    const display::DisplayTiming& timing = *edid_timing;
    int32_t width = timing.horizontal_active_px;
    int32_t height = timing.vertical_active_lines;
    bool duplicate = false;
    for (const display::DisplayTiming& existing_timing : info->edid->timings) {
      if (existing_timing.vertical_field_refresh_rate_millihertz() ==
              timing.vertical_field_refresh_rate_millihertz() &&
          existing_timing.horizontal_active_px == width &&
          existing_timing.vertical_active_lines == height) {
        duplicate = true;
        break;
      }
    }
    if (duplicate) {
      continue;
    }

    layer_t& test_layer = test_layers[0];
    ZX_DEBUG_ASSERT_MSG(
        static_cast<const layer_t*>(&test_layer) == &test_configs[0].layer_list[0],
        "test_layer should be a non-const alias for the first layer in test_configs");
    test_layer.cfg.primary.image_metadata.width = width;
    test_layer.cfg.primary.image_metadata.height = height;
    test_layer.cfg.primary.image_source.width = width;
    test_layer.cfg.primary.image_source.height = height;
    test_layer.cfg.primary.display_destination.width = width;
    test_layer.cfg.primary.display_destination.height = height;

    display_config_t& test_config = test_configs[0];
    test_config.mode = display::ToBanjoDisplayMode(timing);

    uint32_t display_cfg_result;
    client_composition_opcode_t layer_result = 0;
    size_t display_layer_results_count;
    display_cfg_result = engine_driver_client_->CheckConfiguration(
        test_configs, 1, &layer_result,
        /*client_composition_opcodes_count=*/1, &display_layer_results_count);
    if (display_cfg_result == CONFIG_CHECK_RESULT_OK) {
      fbl::AllocChecker ac;
      info->edid->timings.push_back(timing, &ac);
      if (!ac.check()) {
        FDF_LOG(WARNING, "Edid skip allocation failed");
        break;
      }
    }
  }
}

zx::result<> Controller::AddDisplay(const raw_display_info_t& banjo_display_info) {
  zx::result<fbl::RefPtr<DisplayInfo>> display_info_result =
      DisplayInfo::Create(banjo_display_info);
  if (display_info_result.is_error()) {
    FDF_LOG(WARNING, "Failed to add display %" PRIu64 ": %s", banjo_display_info.display_id,
            display_info_result.status_string());
    return display_info_result.take_error();
  }

  fbl::RefPtr<DisplayInfo> display_info = std::move(display_info_result).value();
  DisplayId display_id = display_info->id;

  fbl::AutoLock lock(mtx());
  auto display_it = displays_.find(display_id);
  if (display_it != displays_.end()) {
    FDF_LOG(WARNING, "Display %" PRIu64 " is already created; add display request ignored",
            display_id.value());
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }
  displays_.insert(display_info);

  fbl::AllocChecker alloc_checker;
  auto post_task_state = fbl::make_unique_checked<DisplayTaskState>(&alloc_checker);
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "No memory when processing hotplug");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> post_task_result =
      PostTask(std::move(post_task_state), *client_dispatcher()->async_dispatcher(),
               [this, display_info = std::move(display_info)]() {
                 if (display_info->edid.has_value()) {
                   PopulateDisplayTimings(display_info);
                 }

                 // TODO(b/317914671): Pass parsed display metadata to driver.

                 fbl::AutoLock lock(mtx());

                 const std::array<DisplayId, 1> added_id_candidates = {display_info->id};
                 cpp20::span<const DisplayId> added_ids(added_id_candidates);

                 // TODO(https://fxbug.dev/339311596): Do not trigger the client's
                 // `OnDisplaysChanged` if an added display is ignored.
                 //
                 // Dropping some add events can result in spurious removes, but
                 // those are filtered out in the clients.
                 if (!display_info->edid.has_value() || !display_info->edid->timings.is_empty()) {
                   display_info->init_done = true;
                   display_info->InitializeInspect(&root_);
                 } else {
                   FDF_LOG(WARNING, "Ignoring display with no compatible edid timings");
                   added_ids = {};
                 }

                 if (virtcon_client_ready_) {
                   ZX_DEBUG_ASSERT(virtcon_client_ != nullptr);
                   virtcon_client_->OnDisplaysChanged(added_ids, /*removed_display_ids=*/{});
                 }
                 if (primary_client_ready_) {
                   ZX_DEBUG_ASSERT(primary_client_ != nullptr);
                   primary_client_->OnDisplaysChanged(added_ids, /*removed_display_ids=*/{});
                 }
               });

  if (post_task_result.is_error()) {
    FDF_LOG(ERROR, "Failed to dispatch task: %s", post_task_result.status_string());
  }
  return post_task_result;
}

zx::result<> Controller::RemoveDisplay(DisplayId display_id) {
  fbl::AutoLock lock(mtx());
  fbl::RefPtr<DisplayInfo> removed_display = displays_.erase(display_id);
  if (!removed_display) {
    FDF_LOG(WARNING, "Unknown display %" PRIu64 " removed", display_id.value());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  while (fbl::RefPtr<Image> image = removed_display->images.pop_front()) {
    AssertMtxAliasHeld(*image->mtx());
    image->StartRetire();
    image->OnRetire();
  }

  fbl::AllocChecker alloc_checker;
  auto post_task_state = fbl::make_unique_checked<DisplayTaskState>(&alloc_checker);
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "No memory when processing hotplug");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  zx::result<> post_task_result = PostTask(
      std::move(post_task_state), *client_dispatcher()->async_dispatcher(), [this, display_id]() {
        fbl::AutoLock lock(mtx());
        const std::array<DisplayId, 1> removed_display_ids = {
            display_id,
        };
        if (virtcon_client_ready_) {
          ZX_DEBUG_ASSERT(virtcon_client_ != nullptr);
          virtcon_client_->OnDisplaysChanged(/*added_display_ids=*/{}, removed_display_ids);
        }
        if (primary_client_ready_) {
          ZX_DEBUG_ASSERT(primary_client_ != nullptr);
          primary_client_->OnDisplaysChanged(/*added_display_ids=*/{}, removed_display_ids);
        }
      });

  if (post_task_result.is_error()) {
    FDF_LOG(ERROR, "Failed to dispatch task: %s", post_task_result.status_string());
  }
  return post_task_result;
}

void Controller::DisplayEngineListenerOnDisplayAdded(const raw_display_info_t* banjo_display_info) {
  ZX_DEBUG_ASSERT(banjo_display_info != nullptr);
  zx::result<> added_display_result = AddDisplay(*banjo_display_info);
  if (added_display_result.is_error()) {
    FDF_LOG(WARNING, "Failed to add a display %" PRIu64 ": %s", banjo_display_info->display_id,
            added_display_result.status_string());
  }
}

void Controller::DisplayEngineListenerOnDisplayRemoved(uint64_t banjo_display_id) {
  DisplayId display_id = ToDisplayId(banjo_display_id);
  zx::result<> remove_display_result = RemoveDisplay(display_id);
  if (remove_display_result.is_error()) {
    FDF_LOG(WARNING, "Failed to remove a display: %s", remove_display_result.status_string());
  }
}

void Controller::DisplayEngineListenerOnCaptureComplete() {
  if (!supports_capture_) {
    FDF_LOG(ERROR,
            "OnCaptureComplete(): the display engine doesn't support "
            "display capture.");
    return;
  }

  zx::result<> post_task_result =
      PostTask<kDisplayTaskTargetSize>(*client_dispatcher()->async_dispatcher(), [this]() {
        // Free an image that was previously used by the hardware.
        if (pending_release_capture_image_id_ != kInvalidDriverCaptureImageId) {
          ReleaseCaptureImage(pending_release_capture_image_id_);
          pending_release_capture_image_id_ = kInvalidDriverCaptureImageId;
        }

        fbl::AutoLock lock(mtx());
        if (virtcon_client_ready_) {
          ZX_DEBUG_ASSERT(virtcon_client_ != nullptr);
          virtcon_client_->OnCaptureComplete();
        }
        if (primary_client_ready_) {
          ZX_DEBUG_ASSERT(primary_client_ != nullptr);
          primary_client_->OnCaptureComplete();
        }
      });
  if (post_task_result.is_error()) {
    FDF_LOG(ERROR, "Failed to dispatch capture complete task: %s",
            post_task_result.status_string());
  }
}

void Controller::DisplayEngineListenerOnDisplayVsync(uint64_t banjo_display_id,
                                                     zx_time_t banjo_timestamp,
                                                     const config_stamp_t* banjo_config_stamp_ptr) {
  // Emit an event called "VSYNC", which is by convention the event
  // that Trace Viewer looks for in its "Highlight VSync" feature.
  TRACE_INSTANT("gfx", "VSYNC", TRACE_SCOPE_THREAD, "display_id", banjo_display_id);
  TRACE_DURATION("gfx", "Display::Controller::OnDisplayVsync", "display_id", banjo_display_id);

  const DisplayId display_id(banjo_display_id);

  zx::time vsync_timestamp = zx::time(banjo_timestamp);
  ConfigStamp vsync_config_stamp =
      banjo_config_stamp_ptr ? ToConfigStamp(*banjo_config_stamp_ptr) : kInvalidConfigStamp;
  vsync_monitor_.OnVsync(vsync_timestamp, vsync_config_stamp);

  fbl::AutoLock lock(mtx());
  DisplayInfo* info = nullptr;
  for (auto& display_config : displays_) {
    if (display_config.id == display_id) {
      info = &display_config;
      break;
    }
  }

  if (!info) {
    FDF_LOG(ERROR, "No such display %" PRIu64, display_id.value());
    return;
  }

  // See ::ApplyConfig for more explanation of how vsync image tracking works.
  //
  // If there's a pending layer change, don't process any present/retire actions
  // until the change is complete.
  if (info->pending_layer_change) {
    bool done = vsync_config_stamp >= info->pending_layer_change_controller_config_stamp;
    if (done) {
      info->pending_layer_change = false;
      info->pending_layer_change_controller_config_stamp = kInvalidConfigStamp;
      info->switching_client = false;

      if (active_client_ && info->delayed_apply) {
        active_client_->ReapplyConfig();
      }
    }
  }

  // Determine whether the configuration (associated with Controller
  // |config_stamp|) comes from primary client, virtcon client, or neither.
  enum class ConfigStampSource { kPrimary, kVirtcon, kNeither };
  ConfigStampSource config_stamp_source = ConfigStampSource::kNeither;

  struct {
    ClientProxy* client;
    ConfigStampSource source;
  } const kClientInfo[] = {
      {
          .client = primary_client_,
          .source = ConfigStampSource::kPrimary,
      },
      {
          .client = virtcon_client_,
          .source = ConfigStampSource::kVirtcon,
      },
  };

  for (const auto& [client, source] : kClientInfo) {
    if (client) {
      auto pending_stamps = client->pending_applied_config_stamps();
      auto it = std::find_if(pending_stamps.begin(), pending_stamps.end(),
                             [vsync_config_stamp](const auto& pending_stamp) {
                               return pending_stamp.controller_stamp >= vsync_config_stamp;
                             });
      if (it != pending_stamps.end() && it->controller_stamp == vsync_config_stamp) {
        config_stamp_source = source;
        // Obsolete stamps will be removed in |Client::OnDisplayVsync|.
        break;
      }
    }
  };

  if (!info->pending_layer_change) {
    // Each image in the `info->images` set can fall into one of the following
    // cases:
    // - being displayed (its `latest_controller_config_stamp` matches the
    //   incoming `controller_config_stamp` from display driver);
    // - older than the current displayed image (its
    //   `latest_controller_config_stamp` is less than the incoming
    //   `controller_config_stamp`) and should be retired;
    // - newer than the current displayed image (its
    //   `latest_controller_config_stamp` is greater than the incoming
    //   `controller_config_stamp`) and yet to be presented.
    for (auto it = info->images.begin(); it != info->images.end();) {
      bool should_retire = it->latest_controller_config_stamp() < vsync_config_stamp;

      // Retire any images which are older than whatever is currently in their
      // layer.
      if (should_retire) {
        fbl::RefPtr<Image> image_to_retire = info->images.erase(it++);

        AssertMtxAliasHeld(*image_to_retire->mtx());
        image_to_retire->OnRetire();
        // Older images may not be presented. Ending their flows here
        // ensures the correctness of traces.
        //
        // NOTE: If changing this flow name or ID, please also do so in the
        // corresponding FLOW_BEGIN in display_swapchain.cc.
        TRACE_FLOW_END("gfx", "present_image", image_to_retire->id.value());
      } else {
        it++;
      }
    }
  }

  // TODO(https://fxbug.dev/42152065): This is a stopgap solution to support existing
  // OnVsync() DisplayController FIDL events. In the future we'll remove this
  // logic and only return config seqnos in OnVsync() events instead.

  if (vsync_config_stamp != kInvalidConfigStamp) {
    auto& config_image_queue = info->config_image_queue;

    // Evict retired configurations from the queue.
    while (!config_image_queue.empty() &&
           config_image_queue.front().config_stamp < vsync_config_stamp) {
      config_image_queue.pop();
    }

    // Since the stamps sent from Controller to drivers are in chronological
    // order, the Vsync signals Controller receives should also be in
    // chronological order as well.
    //
    // Applying empty configs won't create entries in |config_image_queue|.
    // Otherwise, we'll get the list of images used at ApplyConfig() with
    // the given |config_stamp|.
    if (!config_image_queue.empty() &&
        config_image_queue.front().config_stamp == vsync_config_stamp) {
      for (const auto& image : config_image_queue.front().images) {
        // End of the flow for the image going to be presented.
        //
        // NOTE: If changing this flow name or ID, please also do so in the
        // corresponding FLOW_BEGIN in display_swapchain.cc.
        TRACE_FLOW_END("gfx", "present_image", image.image_id.value());
      }
    }
  }

  switch (config_stamp_source) {
    case ConfigStampSource::kPrimary:
      primary_client_->OnDisplayVsync(display_id, banjo_timestamp, vsync_config_stamp);
      break;
    case ConfigStampSource::kVirtcon:
      virtcon_client_->OnDisplayVsync(display_id, banjo_timestamp, vsync_config_stamp);
      break;
    case ConfigStampSource::kNeither:
      if (primary_client_) {
        // A previous client applied a config and then disconnected before the vsync. Don't send
        // garbage image IDs to the new primary client.
        if (primary_client_->client_id() != applied_client_id_) {
          FDF_LOG(DEBUG,
                  "Dropping vsync. This was meant for client[%" PRIu64 "], but client[%" PRIu64
                  "] is currently active.\n",
                  applied_client_id_.value(), primary_client_->client_id().value());
        }
      }
  }
}

void Controller::ApplyConfig(DisplayConfig* configs[], int32_t count, ConfigStamp config_stamp,
                             uint32_t layer_stamp, ClientId client_id) {
  zx_time_t timestamp = zx_clock_get_monotonic();
  last_valid_apply_config_timestamp_ns_property_.Set(timestamp);
  last_valid_apply_config_interval_ns_property_.Set(timestamp - last_valid_apply_config_timestamp_);
  last_valid_apply_config_timestamp_ = timestamp;

  last_valid_apply_config_config_stamp_property_.Set(config_stamp.value());

  // TODO(https://fxbug.dev/42080631): Replace VLA with fixed-size array once we have a
  // limit on the number of connected displays.
  const int32_t display_configs_size = std::max(1, count);
  display_config_t display_configs[display_configs_size];
  uint32_t display_count = 0;

  // The applied configuration's stamp.
  //
  // Populated from `controller_stamp_` while the mutex is held.
  ConfigStamp applied_config_stamp = {};

  {
    fbl::AutoLock lock(mtx());
    bool switching_client = client_id != applied_client_id_;

    // The fact that there could already be a vsync waiting to be handled when a config
    // is applied means that a vsync with no handle for a layer could be interpreted as either
    // nothing in the layer has been presented or everything in the layer can be retired. To
    // prevent that ambiguity, we don't allow a layer to be disabled until an image from
    // it has been displayed.
    //
    // Since layers can be moved between displays but the implementation only supports
    // tracking the image in one display's queue, we need to ensure that the old display is
    // done with a migrated image before the new display is done with it. This means
    // that the new display can't flip until the configuration change is done. However, we
    // don't want to completely prohibit flips, as that would add latency if the layer's new
    // image is being waited for when the configuration is applied.
    //
    // To handle both of these cases, we force all layer changes to complete before the client
    // can apply a new configuration. We allow the client to apply a more complete version of
    // the configuration, although Client::HandleApplyConfig won't migrate a layer's current
    // image if there is also a pending image.
    if (switching_client || applied_layer_stamp_ != layer_stamp) {
      for (int i = 0; i < count; i++) {
        DisplayConfig* config = configs[i];
        auto display = displays_.find(config->id);
        if (!display.IsValid()) {
          continue;
        }

        if (display->pending_layer_change) {
          display->delayed_apply = true;
          return;
        }
      }
    }

    // Now we can guarantee that this configuration will be applied to display
    // controller. Thus increment the controller ApplyConfiguration() counter.
    ++controller_stamp_;
    applied_config_stamp = controller_stamp_;

    for (int i = 0; i < count; i++) {
      auto* config = configs[i];
      auto display = displays_.find(config->id);
      if (!display.IsValid()) {
        continue;
      }

      auto& config_image_queue = display->config_image_queue;
      config_image_queue.push({.config_stamp = applied_config_stamp, .images = {}});

      display->switching_client = switching_client;
      display->pending_layer_change = config->apply_layer_change();
      if (display->pending_layer_change) {
        display->pending_layer_change_controller_config_stamp = applied_config_stamp;
      }
      display->layer_count = config->current_layer_count();
      display->delayed_apply = false;

      if (display->layer_count == 0) {
        continue;
      }

      display_configs[display_count++] = *config->current_config();

      for (auto& layer_node : config->get_current_layers()) {
        Layer* layer = layer_node.layer;
        fbl::RefPtr<Image> image = layer->current_image();

        if (layer->is_skipped() || !image) {
          continue;
        }

        // Set the image controller config stamp so vsync knows what config the
        // image was used at.
        AssertMtxAliasHeld(*image->mtx());
        image->set_latest_controller_config_stamp(applied_config_stamp);
        image->StartPresent();

        // It's possible that the image's layer was moved between displays. The logic around
        // pending_layer_change guarantees that the old display will be done with the image
        // before the new display is, so deleting it from the old list is fine.
        //
        // Even if we're on the same display, the entry needs to be moved to the end of the
        // list to ensure that the last config->current.layer_count elements in the queue
        // are the current images.
        if (image->InDoublyLinkedList()) {
          image->RemoveFromDoublyLinkedList();
        }
        display->images.push_back(image);

        config_image_queue.back().images.push_back({image->id, image->client_id()});
      }
    }

    applied_layer_stamp_ = layer_stamp;
    applied_client_id_ = client_id;

    if (active_client_) {
      if (switching_client) {
        active_client_->ReapplySpecialConfigs();
      }

      active_client_->UpdateConfigStampMapping({
          .controller_stamp = controller_stamp_,
          .client_stamp = config_stamp,
      });
    }
  }

  const config_stamp_t banjo_config_stamp = ToBanjoConfigStamp(applied_config_stamp);
  engine_driver_client_->ApplyConfiguration(display_configs, display_count, &banjo_config_stamp);
}

void Controller::ReleaseImage(DriverImageId driver_image_id) {
  engine_driver_client_->ReleaseImage(driver_image_id);
}

void Controller::ReleaseCaptureImage(DriverCaptureImageId driver_capture_image_id) {
  if (!supports_capture_) {
    return;
  }
  if (driver_capture_image_id == kInvalidDriverCaptureImageId) {
    return;
  }

  const zx::result<> result = engine_driver_client_->ReleaseCapture(driver_capture_image_id);
  if (result.is_error() && result.error_value() == ZX_ERR_SHOULD_WAIT) {
    ZX_DEBUG_ASSERT_MSG(pending_release_capture_image_id_ == kInvalidDriverCaptureImageId,
                        "multiple pending releases for capture images");
    // Delay the image release until the hardware is done.
    pending_release_capture_image_id_ = driver_capture_image_id;
  }
}

void Controller::SetVirtconMode(fuchsia_hardware_display::wire::VirtconMode virtcon_mode) {
  fbl::AutoLock lock(mtx());
  virtcon_mode_ = virtcon_mode;
  HandleClientOwnershipChanges();
}

void Controller::HandleClientOwnershipChanges() {
  ClientProxy* new_active;
  if (virtcon_mode_ == fidl_display::wire::VirtconMode::kForced ||
      (virtcon_mode_ == fidl_display::wire::VirtconMode::kFallback && primary_client_ == nullptr)) {
    new_active = virtcon_client_;
  } else {
    new_active = primary_client_;
  }

  if (new_active != active_client_) {
    if (active_client_) {
      active_client_->SetOwnership(false);
    }
    if (new_active) {
      new_active->SetOwnership(true);
    }
    active_client_ = new_active;
  }
}

void Controller::OnClientDead(ClientProxy* client) {
  FDF_LOG(DEBUG, "Client %" PRIu64 " dead", client->client_id().value());
  fbl::AutoLock lock(mtx());
  if (unbinding_) {
    return;
  }
  if (client == virtcon_client_) {
    virtcon_client_ = nullptr;
    virtcon_mode_ = fidl_display::wire::VirtconMode::kInactive;
    virtcon_client_ready_ = false;
  } else if (client == primary_client_) {
    primary_client_ = nullptr;
    primary_client_ready_ = false;
  } else {
    ZX_DEBUG_ASSERT_MSG(false, "Dead client is neither vc nor primary\n");
  }
  HandleClientOwnershipChanges();

  clients_.remove_if(
      [client](std::unique_ptr<ClientProxy>& list_client) { return list_client.get() == client; });
}

zx::result<cpp20::span<const DisplayTiming>> Controller::GetDisplayTimings(DisplayId display_id) {
  if (unbinding_) {
    return zx::error(ZX_ERR_BAD_STATE);
  }
  for (auto& display : displays_) {
    if (display.id == display_id) {
      if (display.edid.has_value()) {
        return zx::ok(cpp20::span(display.edid->timings));
      }

      ZX_DEBUG_ASSERT(display.mode.has_value());
      return zx::ok(cpp20::span(&*display.mode, 1));
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<fbl::Array<CoordinatorPixelFormat>> Controller::GetSupportedPixelFormats(
    DisplayId display_id) {
  fbl::Array<CoordinatorPixelFormat> formats_out;
  for (auto& display : displays_) {
    if (display.id == display_id) {
      fbl::AllocChecker alloc_checker;
      size_t size = display.pixel_formats.size();
      formats_out = fbl::Array<CoordinatorPixelFormat>(
          new (&alloc_checker) CoordinatorPixelFormat[size], size);
      if (!alloc_checker.check()) {
        return zx::error(ZX_ERR_NO_MEMORY);
      }
      std::copy(display.pixel_formats.begin(), display.pixel_formats.end(), formats_out.begin());
      return zx::ok(std::move(formats_out));
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

namespace {

void PrintChannelKoids(ClientPriority client_priority, const zx::channel& channel) {
  zx_info_handle_basic_t info{};
  size_t actual, avail;
  zx_status_t status = channel.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), &actual, &avail);
  if (status != ZX_OK || info.type != ZX_OBJ_TYPE_CHANNEL) {
    FDF_LOG(DEBUG, "Could not get koids for handle(type=%d): %d", info.type, status);
    return;
  }
  ZX_DEBUG_ASSERT(actual == avail);
  FDF_LOG(INFO, "%s client connecting on channel (c=0x%lx, s=0x%lx)",
          DebugStringFromClientPriority(client_priority), info.related_koid, info.koid);
}

}  // namespace

zx_status_t Controller::CreateClient(
    ClientPriority client_priority,
    fidl::ServerEnd<fidl_display::Coordinator> coordinator_server_end,
    fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener> coordinator_listener_client_end,
    fit::function<void()> on_client_disconnected) {
  ZX_DEBUG_ASSERT(on_client_disconnected);

  PrintChannelKoids(client_priority, coordinator_server_end.channel());

  fbl::AllocChecker alloc_checker;
  auto post_task_state = fbl::make_unique_checked<DisplayTaskState>(&alloc_checker);
  if (!alloc_checker.check()) {
    FDF_LOG(DEBUG, "Failed to alloc client task");
    return ZX_ERR_NO_MEMORY;
  }

  fbl::AutoLock lock(mtx());
  if (unbinding_) {
    FDF_LOG(DEBUG, "Client connected during unbind");
    return ZX_ERR_UNAVAILABLE;
  }

  if ((client_priority == ClientPriority::kVirtcon && virtcon_client_ != nullptr) ||
      (client_priority == ClientPriority::kPrimary && primary_client_ != nullptr)) {
    FDF_LOG(DEBUG, "%s client already bound", DebugStringFromClientPriority(client_priority));
    return ZX_ERR_ALREADY_BOUND;
  }

  ClientId client_id = next_client_id_;
  ++next_client_id_;
  auto client = std::make_unique<ClientProxy>(this, client_priority, client_id,
                                              std::move(on_client_disconnected));

  zx_status_t status = client->Init(&root_, std::move(coordinator_server_end),
                                    std::move(coordinator_listener_client_end));
  if (status != ZX_OK) {
    FDF_LOG(DEBUG, "Failed to init client %d", status);
    return status;
  }

  ClientProxy* client_ptr = client.get();
  clients_.push_back(std::move(client));

  FDF_LOG(DEBUG, "New %s client [%" PRIu64 "] connected.",
          DebugStringFromClientPriority(client_priority), client_ptr->client_id().value());

  switch (client_priority) {
    case ClientPriority::kVirtcon:
      ZX_DEBUG_ASSERT(virtcon_client_ == nullptr);
      ZX_DEBUG_ASSERT(!virtcon_client_ready_);
      virtcon_client_ = client_ptr;
      break;
    case ClientPriority::kPrimary:
      ZX_DEBUG_ASSERT(primary_client_ == nullptr);
      ZX_DEBUG_ASSERT(!primary_client_ready_);
      primary_client_ = client_ptr;
  }
  HandleClientOwnershipChanges();

  zx::result<> post_task_result = PostTask(
      std::move(post_task_state), *client_dispatcher()->async_dispatcher(), [this, client_id]() {
        fbl::AutoLock lock(mtx());
        if (unbinding_) {
          return;
        }

        ClientProxy* client_proxy;
        if (virtcon_client_ != nullptr && virtcon_client_->client_id() == client_id) {
          client_proxy = virtcon_client_;
        } else if (primary_client_ != nullptr && primary_client_->client_id() == client_id) {
          client_proxy = primary_client_;
        } else {
          return;
        }

        // Add all existing displays to the client
        if (displays_.size() > 0) {
          DisplayId current_displays[displays_.size()];
          int initialized_display_count = 0;
          for (const DisplayInfo& display : displays_) {
            if (display.init_done) {
              current_displays[initialized_display_count] = display.id;
              ++initialized_display_count;
            }
          }
          cpp20::span<DisplayId> removed_display_ids = {};
          client_proxy->OnDisplaysChanged(
              cpp20::span<DisplayId>(current_displays, initialized_display_count),
              removed_display_ids);
        }

        if (virtcon_client_ == client_proxy) {
          ZX_DEBUG_ASSERT(!virtcon_client_ready_);
          virtcon_client_ready_ = true;
        } else {
          ZX_DEBUG_ASSERT(primary_client_ == client_proxy);
          ZX_DEBUG_ASSERT(!primary_client_ready_);
          primary_client_ready_ = true;
        }
      });
  return post_task_result.status_value();
}

display::DriverBufferCollectionId Controller::GetNextDriverBufferCollectionId() {
  fbl::AutoLock lock(mtx());
  return next_driver_buffer_collection_id_++;
}

void Controller::OpenCoordinatorWithListenerForVirtcon(
    OpenCoordinatorWithListenerForVirtconRequestView request,
    OpenCoordinatorWithListenerForVirtconCompleter::Sync& completer) {
  ZX_DEBUG_ASSERT(request->has_coordinator());
  ZX_DEBUG_ASSERT(request->has_coordinator_listener());
  zx_status_t create_status =
      CreateClient(ClientPriority::kVirtcon, std::move(request->coordinator()),
                   std::move(request->coordinator_listener()),
                   /*on_client_disconnected=*/[] {});
  if (create_status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(create_status);
  }
}

void Controller::OpenCoordinatorWithListenerForPrimary(
    OpenCoordinatorWithListenerForPrimaryRequestView request,
    OpenCoordinatorWithListenerForPrimaryCompleter::Sync& completer) {
  ZX_DEBUG_ASSERT(request->has_coordinator());
  ZX_DEBUG_ASSERT(request->has_coordinator_listener());
  zx_status_t create_status =
      CreateClient(ClientPriority::kPrimary, std::move(request->coordinator()),
                   std::move(request->coordinator_listener()),
                   /*on_client_disconnected=*/[] {});
  if (create_status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(create_status);
  }
}

ConfigStamp Controller::TEST_controller_stamp() const {
  fbl::AutoLock lock(mtx());
  return controller_stamp_;
}

// static
zx::result<std::unique_ptr<Controller>> Controller::Create(
    std::unique_ptr<EngineDriverClient> engine_driver_client,
    fdf::UnownedSynchronizedDispatcher dispatcher) {
  fbl::AllocChecker alloc_checker;

  auto controller = fbl::make_unique_checked<Controller>(
      &alloc_checker, std::move(engine_driver_client), std::move(dispatcher));
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for Controller");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> initialize_result = controller->Initialize();
  if (initialize_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize the Controller device: %s",
            initialize_result.status_string());
    return initialize_result.take_error();
  }

  return zx::ok(std::move(controller));
}

zx::result<> Controller::Initialize() {
  zx::result<> vsync_monitor_init_result = vsync_monitor_.Initialize();
  if (vsync_monitor_init_result.is_error()) {
    // VsyncMonitor::Init() logged the error.
    return vsync_monitor_init_result.take_error();
  }

  engine_driver_client_->RegisterDisplayEngineListener({
      .ops = &display_engine_listener_protocol_ops_,
      .ctx = this,
  });

  supports_capture_ = engine_driver_client_->IsCaptureSupported();
  FDF_LOG(INFO, "Display capture is%s supported", supports_capture_ ? "" : " not");

  return zx::ok();
}

void Controller::PrepareStop() {
  FDF_LOG(INFO, "Controller::PrepareStop");

  fbl::AutoLock lock(mtx());
  unbinding_ = true;
  // Tell each client to start releasing. We know `clients_` will not be
  // modified here because we are holding the lock.
  for (auto& client : clients_) {
    client->CloseOnControllerLoop();
  }

  vsync_monitor_.Deinitialize();

  // Set an empty config so that the display driver releases resources.
  display_config_t empty_config;
  ++controller_stamp_;
  const config_stamp_t banjo_config_stamp = ToBanjoConfigStamp(controller_stamp_);
  engine_driver_client_->ApplyConfiguration(&empty_config, 0, &banjo_config_stamp);

  // It's possible that the Vsync with the null configuration is never
  // triggered when drivers are shut down. We should proactively retire
  // all images on all displays.
  for (DisplayInfo& display : displays_) {
    while (fbl::RefPtr<Image> image = display.images.pop_front()) {
      AssertMtxAliasHeld(*image->mtx());
      image->StartRetire();
      image->OnRetire();
    }
  }

  // Deregister the Controller itself from the display engine driver while both
  // drivers are still alive.
  engine_driver_client_->DeregisterDisplayEngineListener();
}

void Controller::Stop() { FDF_LOG(INFO, "Controller::Stop"); }

Controller::Controller(std::unique_ptr<EngineDriverClient> engine_driver_client,
                       fdf::UnownedSynchronizedDispatcher dispatcher)
    : Controller(std::move(engine_driver_client), std::move(dispatcher), inspect::Inspector{}) {
  ZX_DEBUG_ASSERT(engine_driver_client_ != nullptr);
}

Controller::Controller(std::unique_ptr<EngineDriverClient> engine_driver_client,
                       fdf::UnownedSynchronizedDispatcher client_dispatcher,
                       inspect::Inspector inspector)
    : inspector_(std::move(inspector)),
      root_(inspector_.GetRoot().CreateChild("display")),
      client_dispatcher_(std::move(client_dispatcher)),
      vsync_monitor_(root_.CreateChild("vsync_monitor"), client_dispatcher_->async_dispatcher()),
      engine_driver_client_(std::move(engine_driver_client)) {
  ZX_DEBUG_ASSERT(engine_driver_client_ != nullptr);

  last_valid_apply_config_timestamp_ns_property_ =
      root_.CreateUint("last_valid_apply_config_timestamp_ns", 0);
  last_valid_apply_config_interval_ns_property_ =
      root_.CreateUint("last_valid_apply_config_interval_ns", 0);
  last_valid_apply_config_config_stamp_property_ =
      root_.CreateUint("last_valid_apply_config_stamp", kInvalidConfigStamp.value());
}

Controller::~Controller() { FDF_LOG(INFO, "Controller::~Controller"); }

size_t Controller::TEST_imported_images_count() const {
  fbl::AutoLock lock(mtx());
  size_t virtcon_images = virtcon_client_ ? virtcon_client_->TEST_imported_images_count() : 0;
  size_t primary_images = primary_client_ ? primary_client_->TEST_imported_images_count() : 0;
  size_t display_images = 0;
  for (const auto& display : displays_) {
    display_images += display.images.size_slow();
  }
  return virtcon_images + primary_images + display_images;
}

}  // namespace display
