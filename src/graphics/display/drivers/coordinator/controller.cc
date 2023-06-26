// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/controller.h"

#include <fuchsia/hardware/display/clamprgb/cpp/banjo.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/trace/event.h>
#include <lib/zbi-format/graphics.h>
#include <threads.h>
#include <zircon/syscalls.h>
#include <zircon/threads.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <algorithm>
#include <cinttypes>
#include <cstdint>
#include <iterator>
#include <memory>
#include <optional>
#include <utility>
#include <vector>

#include <audio-proto-utils/format-utils.h>
#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/auto_lock.h>
#include <fbl/ref_ptr.h>
#include <fbl/string_printf.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/coordinator/client.h"
#include "src/graphics/display/drivers/coordinator/display-info.h"
#include "src/graphics/display/drivers/coordinator/eld.h"
#include "src/graphics/display/drivers/coordinator/migration-util.h"
#include "src/graphics/display/lib/api-types-cpp/capture-image-id.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-capture-image-id.h"

namespace fidl_display = fuchsia_hardware_display;

namespace {

// Use the same default watchdog timeout as scenic, which may help ensure watchdog logs/errors
// happen close together and can be correlated.
constexpr uint64_t kWatchdogWarningIntervalMs = 15000;
constexpr uint64_t kWatchdogTimeoutMs = 45000;

// vsync delivery is considered to be stalled if at least this amount of time has elapsed since
// vsync was last observed.
constexpr zx::duration kVsyncStallThreshold = zx::sec(10);
constexpr zx::duration kVsyncMonitorInterval = kVsyncStallThreshold / 2;

}  // namespace

namespace display {

void Controller::PopulateDisplayMode(const edid::timing_params_t& params, display_mode_t* mode) {
  mode->pixel_clock_10khz = params.pixel_freq_10khz;
  mode->h_addressable = params.horizontal_addressable;
  mode->h_front_porch = params.horizontal_front_porch;
  mode->h_sync_pulse = params.horizontal_sync_pulse;
  mode->h_blanking = params.horizontal_blanking;
  mode->v_addressable = params.vertical_addressable;
  mode->v_front_porch = params.vertical_front_porch;
  mode->v_sync_pulse = params.vertical_sync_pulse;
  mode->v_blanking = params.vertical_blanking;
  mode->flags = params.flags;

  static_assert(MODE_FLAG_VSYNC_POSITIVE == edid::timing_params::kPositiveVsync, "");
  static_assert(MODE_FLAG_HSYNC_POSITIVE == edid::timing_params::kPositiveHsync, "");
  static_assert(MODE_FLAG_INTERLACED == edid::timing_params::kInterlaced, "");
  static_assert(MODE_FLAG_ALTERNATING_VBLANK == edid::timing_params::kAlternatingVblank, "");
  static_assert(MODE_FLAG_DOUBLE_CLOCKED == edid::timing_params::kDoubleClocked, "");
}

void Controller::PopulateDisplayTimings(const fbl::RefPtr<DisplayInfo>& info) {
  if (!info->edid.has_value()) {
    return;
  }

  // Go through all the display mode timings and record whether or not
  // a basic layer configuration is acceptable.
  layer_t test_layer = {};
  layer_t* test_layers[] = {&test_layer};
  test_layer.type = LAYER_TYPE_PRIMARY;
  display_config_t test_config;
  const display_config_t* test_configs[] = {&test_config};
  test_config.display_id = ToBanjoDisplayId(info->id);
  test_config.layer_count = 1;
  test_config.layer_list = test_layers;

  for (auto timing = edid::timing_iterator(&info->edid->base); timing.is_valid(); ++timing) {
    uint32_t width = timing->horizontal_addressable;
    uint32_t height = timing->vertical_addressable;
    bool duplicate = false;
    for (auto& existing_timing : info->edid->timings) {
      if (existing_timing.vertical_refresh_e2 == timing->vertical_refresh_e2 &&
          existing_timing.horizontal_addressable == width &&
          existing_timing.vertical_addressable == height) {
        duplicate = true;
        break;
      }
    }
    if (duplicate) {
      continue;
    }
    test_layer.cfg.primary.image.width = width;
    test_layer.cfg.primary.image.height = height;
    test_layer.cfg.primary.src_frame.width = width;
    test_layer.cfg.primary.src_frame.height = height;
    test_layer.cfg.primary.dest_frame.width = width;
    test_layer.cfg.primary.dest_frame.height = height;
    PopulateDisplayMode(*timing, &test_config.mode);

    uint32_t display_cfg_result;
    uint32_t layer_result = 0;
    size_t display_layer_results_count;
    uint32_t* display_layer_results[] = {&layer_result};
    display_cfg_result = dc_.CheckConfiguration(test_configs, 1, display_layer_results,
                                                &display_layer_results_count);
    if (display_cfg_result == CONFIG_CHECK_RESULT_OK) {
      fbl::AllocChecker ac;
      info->edid->timings.push_back(*timing, &ac);
      if (!ac.check()) {
        zxlogf(WARNING, "Edid skip allocation failed");
        break;
      }
    }
  }
}

void Controller::DisplayControllerInterfaceOnDisplaysChanged(
    const added_display_args_t* displays_added, size_t added_count,
    const uint64_t* displays_removed, size_t removed_count,
    added_display_info_t* out_display_info_list, size_t display_info_count,
    size_t* display_info_actual) {
  ZX_DEBUG_ASSERT(!out_display_info_list || added_count == display_info_count);

  fbl::Vector<fbl::RefPtr<DisplayInfo>> added_display_infos;
  fbl::Vector<DisplayId> removed_display_ids;
  std::unique_ptr<async::Task> task;
  uint32_t added_success_count = 0;

  fbl::AllocChecker alloc_checker;
  if (added_count) {
    added_display_infos.reserve(added_count, &alloc_checker);
    if (!alloc_checker.check()) {
      zxlogf(ERROR, "No memory when processing hotplug");
      return;
    }
  }
  if (removed_count) {
    removed_display_ids.reserve(removed_count, &alloc_checker);
    if (!alloc_checker.check()) {
      zxlogf(ERROR, "No memory when processing hotplug");
      return;
    }
    for (size_t i = 0; i < removed_count; ++i) {
      removed_display_ids.push_back(ToDisplayId(displays_removed[i]));
    }
  }
  task = fbl::make_unique_checked<async::Task>(&alloc_checker);
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "No memory when processing hotplug");
    return;
  }

  fbl::AutoLock lock(mtx());

  for (unsigned i = 0; i < removed_count; i++) {
    DisplayId removed_display_id(displays_removed[i]);

    auto target = displays_.erase(removed_display_id);
    if (target) {
      while (fbl::RefPtr<Image> image = target->images.pop_front()) {
        AssertMtxAliasHeld(image->mtx());
        image->StartRetire();
        image->OnRetire();
      }
    } else {
      zxlogf(DEBUG, "Unknown display %" PRIu64 " removed", removed_display_id.value());
    }
  }

  for (unsigned i = 0; i < added_count; i++) {
    zx::result<fbl::RefPtr<DisplayInfo>> info_result = DisplayInfo::Create(displays_added[i]);
    if (info_result.is_error()) {
      zxlogf(INFO, "failed to add display %ld: %s", displays_added[i].display_id,
             info_result.status_string());
      continue;
    }
    fbl::RefPtr<DisplayInfo> info = std::move(info_result).value();
    if (info->edid.has_value()) {
      fbl::Array<uint8_t> eld;
      ComputeEld(info->edid->base, eld);
      dc_.SetEld(ToBanjoDisplayId(info->id), eld.get(), eld.size());
    }
    auto* display_info = out_display_info_list ? &out_display_info_list[i] : nullptr;
    if (display_info && info->edid.has_value()) {
      const edid::Edid& edid = info->edid->base;
      display_info->is_hdmi_out = edid.is_hdmi();
      display_info->is_standard_srgb_out = edid.is_standard_rgb();
      display_info->audio_format_count = static_cast<uint32_t>(info->edid->audio.size());

      static_assert(
          sizeof(display_info->monitor_name) == sizeof(edid::Descriptor::Monitor::data) + 1,
          "Possible overflow");
      static_assert(
          sizeof(display_info->monitor_name) == sizeof(edid::Descriptor::Monitor::data) + 1,
          "Possible overflow");
      strcpy(display_info->manufacturer_id, edid.manufacturer_id());
      strcpy(display_info->monitor_name, edid.monitor_name());
      strcpy(display_info->monitor_serial, edid.monitor_serial());
      display_info->manufacturer_name = edid.manufacturer_name();
      display_info->horizontal_size_mm = edid.horizontal_size_mm();
      display_info->vertical_size_mm = edid.vertical_size_mm();
    }

    if (displays_.insert_or_find(info)) {
      added_display_infos.push_back(std::move(info));
    } else {
      zxlogf(INFO, "Ignoring duplicate display");
    }
  }
  if (display_info_actual)
    *display_info_actual = added_success_count;

  task->set_handler([this, added_display_infos = std::move(added_display_infos),
                     removed_display_ids = std::move(removed_display_ids)](
                        async_dispatcher_t* dispatcher, async::Task* task, zx_status_t status) {
    if (status == ZX_OK) {
      for (const fbl::RefPtr<DisplayInfo>& added_display_info : added_display_infos) {
        if (added_display_info->edid.has_value()) {
          PopulateDisplayTimings(added_display_info);
        }
      }
      fbl::AutoLock lock(mtx());

      fbl::Vector<DisplayId> added_ids;
      added_ids.reserve(added_display_infos.size());
      for (const fbl::RefPtr<DisplayInfo>& added_display_info : added_display_infos) {
        // Dropping some add events can result in spurious removes, but
        // those are filtered out in the clients.
        if (!added_display_info->edid.has_value() ||
            !added_display_info->edid->timings.is_empty()) {
          added_ids.push_back(added_display_info->id);
          added_display_info->init_done = true;
          added_display_info->InitializeInspect(&root_);
        } else {
          zxlogf(WARNING, "Ignoring display with no compatible edid timings");
        }
      }
      if (vc_client_ && vc_ready_) {
        vc_client_->OnDisplaysChanged(added_ids, removed_display_ids);
      }
      if (primary_client_ && primary_ready_) {
        primary_client_->OnDisplaysChanged(added_ids, removed_display_ids);
      }

    } else {
      zxlogf(ERROR, "Failed to dispatch display change task %d", status);
    }

    delete task;
  });
  task.release()->Post(loop_.dispatcher());
}

void Controller::DisplayCaptureInterfaceOnCaptureComplete() {
  std::unique_ptr<async::Task> task = std::make_unique<async::Task>();
  fbl::AutoLock lock(mtx());
  task->set_handler([this](async_dispatcher_t* dispatcher, async::Task* task, zx_status_t status) {
    if (status == ZX_OK) {
      // Free an image that was previously used by the hardware.
      if (pending_release_capture_image_id_ != kInvalidDriverCaptureImageId) {
        ReleaseCaptureImage(pending_release_capture_image_id_);
        pending_release_capture_image_id_ = kInvalidDriverCaptureImageId;
      }
      fbl::AutoLock lock(mtx());
      if (vc_client_ && vc_ready_) {
        vc_client_->OnCaptureComplete();
      }
      if (primary_client_ && primary_ready_) {
        primary_client_->OnCaptureComplete();
      }
    } else {
      zxlogf(ERROR, "Failed to dispatch capture complete task %d", status);
    }
    delete task;
  });
  task.release()->Post(loop_.dispatcher());
}

void Controller::DisplayControllerInterfaceOnDisplayVsync(uint64_t banjo_display_id,
                                                          zx_time_t timestamp,
                                                          const config_stamp_t* config_stamp_ptr) {
  // Emit an event called "VSYNC", which is by convention the event
  // that Trace Viewer looks for in its "Highlight VSync" feature.
  TRACE_INSTANT("gfx", "VSYNC", TRACE_SCOPE_THREAD, "display_id", banjo_display_id);
  TRACE_DURATION("gfx", "Display::Controller::OnDisplayVsync", "display_id", banjo_display_id);

  const DisplayId display_id(banjo_display_id);

  last_vsync_ns_property_.Set(timestamp);
  last_vsync_interval_ns_property_.Set(timestamp - last_vsync_timestamp_.load().get());
  last_vsync_timestamp_ = zx::time(timestamp);
  vsync_stalled_ = false;

  ConfigStamp controller_config_stamp =
      config_stamp_ptr ? ToConfigStamp(*config_stamp_ptr) : kInvalidConfigStamp;
  last_vsync_config_stamp_property_.Set(controller_config_stamp.value());

  fbl::AutoLock lock(mtx());
  DisplayInfo* info = nullptr;
  for (auto& display_config : displays_) {
    if (display_config.id == display_id) {
      info = &display_config;
      break;
    }
  }

  if (!info) {
    zxlogf(ERROR, "No such display %" PRIu64, display_id.value());
    return;
  }

  // See ::ApplyConfig for more explanation of how vsync image tracking works.
  //
  // If there's a pending layer change, don't process any present/retire actions
  // until the change is complete.
  if (info->pending_layer_change) {
    bool done = controller_config_stamp >= info->pending_layer_change_controller_config_stamp;
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
          .client = vc_client_,
          .source = ConfigStampSource::kVirtcon,
      },
  };

  for (const auto& [client, source] : kClientInfo) {
    if (client) {
      auto pending_stamps = client->pending_applied_config_stamps();
      auto it = std::find_if(pending_stamps.begin(), pending_stamps.end(),
                             [controller_config_stamp](const auto& pending_stamp) {
                               return pending_stamp.controller_stamp >= controller_config_stamp;
                             });
      if (it != pending_stamps.end() && it->controller_stamp == controller_config_stamp) {
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
      bool should_retire = it->latest_controller_config_stamp() < controller_config_stamp;

      // Retire any images which are older than whatever is currently in their
      // layer.
      if (should_retire) {
        fbl::RefPtr<Image> image_to_retire = info->images.erase(it++);

        AssertMtxAliasHeld(image_to_retire->mtx());
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

  // TODO(fxbug.dev/72588): This is a stopgap solution to support existing
  // OnVsync() DisplayController FIDL events. In the future we'll remove this
  // logic and only return config seqnos in OnVsync() events instead.

  if (controller_config_stamp != kInvalidConfigStamp) {
    auto& config_image_queue = info->config_image_queue;

    // Evict retired configurations from the queue.
    while (!config_image_queue.empty() &&
           config_image_queue.front().config_stamp < controller_config_stamp) {
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
        config_image_queue.front().config_stamp == controller_config_stamp) {
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
      primary_client_->OnDisplayVsync(display_id, timestamp, controller_config_stamp);
      break;
    case ConfigStampSource::kVirtcon:
      vc_client_->OnDisplayVsync(display_id, timestamp, controller_config_stamp);
      break;
    case ConfigStampSource::kNeither:
      if (primary_client_) {
        // A previous client applied a config and then disconnected before the vsync. Don't send
        // garbage image IDs to the new primary client.
        if (primary_client_->id() != applied_client_id_) {
          zxlogf(DEBUG,
                 "Dropping vsync. This was meant for client[%d], "
                 "but client[%d] is currently active.\n",
                 applied_client_id_, primary_client_->id());
        }
      }
  }
}

zx_status_t Controller::DisplayControllerInterfaceGetAudioFormat(
    uint64_t banjo_display_id, uint32_t fmt_idx, audio_types_audio_stream_format_range_t* fmt_out) {
  const DisplayId display_id = ToDisplayId(banjo_display_id);

  fbl::AutoLock lock(mtx());
  auto display = displays_.find(display_id);
  if (!display.IsValid()) {
    return ZX_ERR_NOT_FOUND;
  }

  if (!display->edid.has_value()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (fmt_idx > display->edid->audio.size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  *fmt_out = display->edid->audio[fmt_idx];
  return ZX_OK;
}

void Controller::ApplyConfig(DisplayConfig* configs[], int32_t count, bool is_vc,
                             ConfigStamp config_stamp, uint32_t layer_stamp, uint32_t client_id) {
  zx_time_t timestamp = zx_clock_get_monotonic();
  last_valid_apply_config_timestamp_ns_property_.Set(timestamp);
  last_valid_apply_config_interval_ns_property_.Set(timestamp - last_valid_apply_config_timestamp_);
  last_valid_apply_config_timestamp_ = timestamp;

  last_valid_apply_config_config_stamp_property_.Set(config_stamp.value());

  // Release the bootloader framebuffer referenced by the kernel. This only
  // needs to be done once on the first ApplyConfig().
  if (!kernel_framebuffer_released_) {
    // Please do not use get_root_resource() in new code. See fxbug.dev/31358.
    zx_framebuffer_set_range(get_root_resource(), /*vmo=*/ZX_HANDLE_INVALID, /*len=*/0,
                             /*format=*/ZBI_PIXEL_FORMAT_NONE, /*width=*/0, /*height=*/0,
                             /*stride=*/0);
    kernel_framebuffer_released_ = true;
  }

  fbl::Array<const display_config_t*> display_configs(new const display_config_t*[count], count);
  uint32_t display_count = 0;

  // The applied configuration's stamp.
  //
  // Populated from `controller_stamp_` while the mutex is held.
  ConfigStamp applied_config_stamp = {};

  {
    fbl::AutoLock lock(mtx());
    bool switching_client = (is_vc != vc_applied_ || client_id != applied_client_id_);
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

      display_configs[display_count++] = config->current_config();

      for (auto& layer_node : config->get_current_layers()) {
        Layer* layer = layer_node.layer;
        fbl::RefPtr<Image> image = layer->current_image();

        if (layer->is_skipped() || !image) {
          continue;
        }

        // Set the image controller config stamp so vsync knows what config the
        // image was used at.
        AssertMtxAliasHeld(image->mtx());
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

    vc_applied_ = is_vc;
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
  dc_.ApplyConfiguration(display_configs.get(), display_count, &banjo_config_stamp);
}

void Controller::ReleaseImage(Image* image) { dc_.ReleaseImage(&image->info()); }

void Controller::ReleaseCaptureImage(DriverCaptureImageId driver_capture_image_id) {
  if (!supports_capture_) {
    return;
  }
  if (driver_capture_image_id == kInvalidDriverCaptureImageId) {
    return;
  }

  const zx_status_t release_status =
      dc_.ReleaseCapture(ToBanjoDriverCaptureImageId(driver_capture_image_id));
  if (release_status == ZX_ERR_SHOULD_WAIT) {
    ZX_DEBUG_ASSERT_MSG(pending_release_capture_image_id_ == kInvalidDriverCaptureImageId,
                        "multiple pending releases for capture images");
    // Delay the image release until the hardware is done.
    pending_release_capture_image_id_ = driver_capture_image_id;
  }
}

void Controller::SetVcMode(fuchsia_hardware_display::wire::VirtconMode vc_mode) {
  fbl::AutoLock lock(mtx());
  vc_mode_ = vc_mode;
  HandleClientOwnershipChanges();
}

void Controller::HandleClientOwnershipChanges() {
  ClientProxy* new_active;
  if (vc_mode_ == fidl_display::wire::VirtconMode::kForced ||
      (vc_mode_ == fidl_display::wire::VirtconMode::kFallback && primary_client_ == nullptr)) {
    new_active = vc_client_;
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
  zxlogf(DEBUG, "Client %d dead", client->id());
  fbl::AutoLock lock(mtx());
  if (unbinding_) {
    return;
  }
  if (client == vc_client_) {
    vc_client_ = nullptr;
    vc_mode_ = fidl_display::wire::VirtconMode::kInactive;
  } else if (client == primary_client_) {
    primary_client_ = nullptr;
  } else {
    ZX_DEBUG_ASSERT_MSG(false, "Dead client is neither vc nor primary\n");
  }
  HandleClientOwnershipChanges();

  clients_.remove_if(
      [client](std::unique_ptr<ClientProxy>& list_client) { return list_client.get() == client; });
}

bool Controller::GetPanelConfig(DisplayId display_id,
                                const fbl::Vector<edid::timing_params_t>** timings,
                                const display_params_t** params) {
  ZX_DEBUG_ASSERT(mtx_trylock(&mtx_) == thrd_busy);
  if (unbinding_) {
    return false;
  }
  for (auto& display : displays_) {
    if (display.id == display_id) {
      if (display.edid.has_value()) {
        *timings = &display.edid->timings;
        *params = nullptr;
      } else {
        *params = &display.params;
        *timings = nullptr;
      }
      return true;
    }
  }
  return false;
}

zx::result<fbl::Array<CoordinatorCursorInfo>> Controller::GetCursorInfo(DisplayId display_id) {
  ZX_DEBUG_ASSERT(mtx_trylock(&mtx_) == thrd_busy);
  fbl::Array<CoordinatorCursorInfo> cursor_info_out;
  for (auto& display : displays_) {
    if (display.id == display_id) {
      fbl::AllocChecker alloc_checker;
      size_t size = display.cursor_infos.size();
      cursor_info_out =
          fbl::Array<CoordinatorCursorInfo>(new (&alloc_checker) CoordinatorCursorInfo[size], size);
      if (!alloc_checker.check()) {
        return zx::error(ZX_ERR_NO_MEMORY);
      }
      std::copy(display.cursor_infos.begin(), display.cursor_infos.end(), cursor_info_out.begin());
      return zx::ok(std::move(cursor_info_out));
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<fbl::Array<CoordinatorPixelFormat>> Controller::GetSupportedPixelFormats(
    DisplayId display_id) {
  ZX_DEBUG_ASSERT(mtx_trylock(&mtx_) == thrd_busy);
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

bool Controller::GetDisplayIdentifiers(DisplayId display_id, const char** manufacturer_name,
                                       const char** monitor_name, const char** monitor_serial) {
  ZX_DEBUG_ASSERT(mtx_trylock(&mtx_) == thrd_busy);
  for (auto& display : displays_) {
    if (display.id == display_id) {
      display.GetIdentifiers(manufacturer_name, monitor_name, monitor_serial);
      return true;
    }
  }
  return false;
}

bool Controller::GetDisplayPhysicalDimensions(DisplayId display_id, uint32_t* horizontal_size_mm,
                                              uint32_t* vertical_size_mm) {
  ZX_DEBUG_ASSERT(mtx_trylock(&mtx_) == thrd_busy);
  for (DisplayInfo& display : displays_) {
    if (display.id == display_id) {
      display.GetPhysicalDimensions(horizontal_size_mm, vertical_size_mm);
      return true;
    }
  }
  return false;
}

static void PrintChannelKoids(bool is_vc, const zx::channel& channel) {
  zx_info_handle_basic_t info{};
  size_t actual, avail;
  zx_status_t status = channel.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), &actual, &avail);
  if (status != ZX_OK || info.type != ZX_OBJ_TYPE_CHANNEL) {
    zxlogf(DEBUG, "Could not get koids for handle(type=%d): %d", info.type, status);
    return;
  }
  ZX_DEBUG_ASSERT(actual == avail);
  zxlogf(INFO, "%s client connecting on channel (c=0x%lx, s=0x%lx)", is_vc ? "vc" : "dc",
         info.related_koid, info.koid);
}

zx_status_t Controller::CreateClient(
    bool is_vc, fidl::ServerEnd<fidl_display::Coordinator> coordinator_server_end,
    fit::function<void()> on_display_client_dead) {
  PrintChannelKoids(is_vc, coordinator_server_end.channel());
  fbl::AllocChecker ac;
  std::unique_ptr<async::Task> task = fbl::make_unique_checked<async::Task>(&ac);
  if (!ac.check()) {
    zxlogf(DEBUG, "Failed to alloc client task");
    return ZX_ERR_NO_MEMORY;
  }

  fbl::AutoLock lock(mtx());
  if (unbinding_) {
    zxlogf(DEBUG, "Client connected during unbind");
    return ZX_ERR_UNAVAILABLE;
  }

  if ((is_vc && vc_client_) || (!is_vc && primary_client_)) {
    zxlogf(DEBUG, "Already bound");
    return ZX_ERR_ALREADY_BOUND;
  }

  auto client = std::make_unique<ClientProxy>(this, is_vc, next_client_id_++,
                                              std::move(on_display_client_dead));

  zx_status_t status = client->Init(&root_, std::move(coordinator_server_end));
  if (status != ZX_OK) {
    zxlogf(DEBUG, "Failed to init client %d", status);
    return status;
  }

  ClientProxy* client_ptr = client.get();
  clients_.push_back(std::move(client));

  zxlogf(DEBUG, "New %s client [%d] connected.", is_vc ? "dc-vc" : "dc", client_ptr->id());

  if (is_vc) {
    vc_client_ = client_ptr;
    vc_ready_ = false;
  } else {
    primary_client_ = client_ptr;
    primary_ready_ = false;
  }
  HandleClientOwnershipChanges();

  task->set_handler(
      [this, client_ptr](async_dispatcher_t* dispatcher, async::Task* task, zx_status_t status) {
        if (status == ZX_OK) {
          fbl::AutoLock lock(mtx());
          if (unbinding_) {
            return;
          }
          if (client_ptr == vc_client_ || client_ptr == primary_client_) {
            // Add all existing displays to the client
            if (displays_.size() > 0) {
              DisplayId current_displays[displays_.size()];
              int idx = 0;
              for (const DisplayInfo& display : displays_) {
                if (display.init_done) {
                  current_displays[idx++] = display.id;
                }
              }
              cpp20::span<DisplayId> removed_display_ids = {};
              client_ptr->OnDisplaysChanged(
                  cpp20::span<DisplayId>(current_displays, displays_.size()), removed_display_ids);
            }

            if (vc_client_ == client_ptr) {
              vc_ready_ = true;
            } else {
              primary_ready_ = true;
            }
          }
        }
        delete task;
      });

  return task.release()->Post(loop_.dispatcher());
}

display::DriverBufferCollectionId Controller::GetNextDriverBufferCollectionId() {
  fbl::AutoLock lock(mtx());
  return next_driver_buffer_collection_id_++;
}

void Controller::OpenCoordinatorForVirtcon(OpenCoordinatorForVirtconRequestView request,
                                           OpenCoordinatorForVirtconCompleter::Sync& completer) {
  completer.Reply(CreateClient(/*is_vc=*/true, std::move(request->coordinator)));
}

void Controller::OpenCoordinatorForPrimary(OpenCoordinatorForPrimaryRequestView request,
                                           OpenCoordinatorForPrimaryCompleter::Sync& completer) {
  completer.Reply(CreateClient(/*is_vc=*/false, std::move(request->coordinator)));
}

void Controller::OnVsyncMonitor() {
  if (vsync_stalled_) {
    return;
  }

  if ((zx::clock::get_monotonic() - last_vsync_timestamp_.load()) > kVsyncStallThreshold) {
    vsync_stalled_ = true;
    vsync_stalls_detected_.Add(1);
  }

  zx_status_t status = vsync_monitor_.PostDelayed(loop_.dispatcher(), kVsyncMonitorInterval);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to schedule vsync monitor: %s", zx_status_get_string(status));
  }
}

ConfigStamp Controller::TEST_controller_stamp() const {
  fbl::AutoLock lock(mtx());
  return controller_stamp_;
}

zx_status_t Controller::Bind(std::unique_ptr<display::Controller>* device_ptr) {
  ZX_DEBUG_ASSERT_MSG(device_ptr && device_ptr->get() == this, "Wrong controller passed to Bind()");

  zx_status_t status;
  dc_ = ddk::DisplayControllerImplProtocolClient(parent_);
  if (!dc_.is_valid()) {
    ZX_DEBUG_ASSERT_MSG(false, "Display controller bind mismatch");
    return ZX_ERR_NOT_SUPPORTED;
  }

  // optional display controller clamp rgb protocol client
  dc_clamp_rgb_ = ddk::DisplayClampRgbImplProtocolClient(parent_);

  status = loop_.StartThread("display-client-loop", &loop_thread_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to start loop %d", status);
    return status;
  }

  if ((status = DdkAdd(ddk::DeviceAddArgs("display-coordinator")
                           .set_flags(DEVICE_ADD_NON_BINDABLE)
                           .set_inspect_vmo(inspector_.DuplicateVmo()))) != ZX_OK) {
    zxlogf(ERROR, "Failed to add display core device %d", status);
    return status;
  }

  // Set the display controller looper thread to use a scheduler role.
  {
    const char* role_name = "fuchsia.graphics.display.drivers.display.controller";
    status = device_set_profile_by_role(this->zxdev(), thrd_get_zx_handle(loop_thread_), role_name,
                                        strlen(role_name));
    if (status != ZX_OK) {
      zxlogf(WARNING, "Failed to apply role: %s", zx_status_get_string(status));
    }
  }

  [[maybe_unused]] auto ptr = device_ptr->release();

  dc_.SetDisplayControllerInterface(this, &display_controller_interface_protocol_ops_);

  status = dc_.SetDisplayCaptureInterface(this, &display_capture_interface_protocol_ops_);
  supports_capture_ = (status == ZX_OK);
  zxlogf(INFO, "Display capture is%s supported: %s", supports_capture_ ? "" : " not",
         zx_status_get_string(status));

  status = vsync_monitor_.PostDelayed(loop_.dispatcher(), kVsyncMonitorInterval);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to schedule vsync monitor: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

void Controller::DdkUnbind(ddk::UnbindTxn txn) {
  zxlogf(INFO, "Controller::DdkUnbind");

  fbl::AutoLock lock(mtx());
  unbinding_ = true;
  // Tell each client to start releasing. We know `clients_` will not be
  // modified here because we are holding the lock.
  for (auto& client : clients_) {
    client->CloseOnControllerLoop();
  }

  txn.Reply();
}

void Controller::DdkRelease() {
  vsync_monitor_.Cancel();
  // Clients may have active work holding mtx_ in loop_.dispatcher(), so shut it down without mtx_.
  loop_.Shutdown();

  // Set an empty config so that the display driver releases resources.
  const display_config_t* configs;
  {
    fbl::AutoLock lock(mtx());
    ++controller_stamp_;
    const config_stamp_t banjo_config_stamp = ToBanjoConfigStamp(controller_stamp_);
    dc_.ApplyConfiguration(&configs, 0, &banjo_config_stamp);
  }
  delete this;
}

Controller::Controller(zx_device_t* parent)
    : DeviceType(parent),
      loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
      watchdog_("display-client-loop", kWatchdogWarningIntervalMs, kWatchdogTimeoutMs,
                loop_.dispatcher()) {
  mtx_init(&mtx_, mtx_plain);
  root_ = inspector_.GetRoot().CreateChild("display");
  last_vsync_ns_property_ = root_.CreateUint("last_vsync_timestamp_ns", 0);
  last_vsync_interval_ns_property_ = root_.CreateUint("last_vsync_interval_ns", 0);
  last_vsync_config_stamp_property_ =
      root_.CreateUint("last_vsync_config_stamp", kInvalidConfigStamp.value());

  last_valid_apply_config_timestamp_ns_property_ =
      root_.CreateUint("last_valid_apply_config_timestamp_ns", 0);
  last_valid_apply_config_interval_ns_property_ =
      root_.CreateUint("last_valid_apply_config_interval_ns", 0);
  last_valid_apply_config_config_stamp_property_ =
      root_.CreateUint("last_valid_apply_config_stamp", kInvalidConfigStamp.value());

  vsync_stalls_detected_ = root_.CreateUint("vsync_stalls", 0);
}

Controller::~Controller() { zxlogf(INFO, "Controller::~Controller"); }

size_t Controller::TEST_imported_images_count() const {
  fbl::AutoLock lock(mtx());
  size_t vc_images = vc_client_ ? vc_client_->TEST_imported_images_count() : 0;
  size_t primary_images = primary_client_ ? primary_client_->TEST_imported_images_count() : 0;
  size_t display_images = 0;
  for (const auto& display : displays_) {
    display_images += display.images.size_slow();
  }
  return vc_images + primary_images + display_images;
}

// ControllerInstance methods

}  // namespace display

static zx_status_t display_controller_bind(void* ctx, zx_device_t* parent) {
  fbl::AllocChecker ac;
  std::unique_ptr<display::Controller> core(new (&ac) display::Controller(parent));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  return core->Bind(&core);
}

static constexpr zx_driver_ops_t display_controller_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = display_controller_bind;
  return ops;
}();

ZIRCON_DRIVER(display_controller, display_controller_ops, "zircon", "0.1");
