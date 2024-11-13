// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-display/intel-display.h"

#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <fuchsia/hardware/intelgpucore/c/banjo.h>
#include <lib/ddk/hw/inout.h>
#include <lib/device-protocol/pci.h>
#include <lib/driver/component/cpp/prepare_stop_completer.h>
#include <lib/driver/component/cpp/start_completer.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/image-format/image_format.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zbitl/items/graphics.h>
#include <lib/zx/resource.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <algorithm>
#include <cstdlib>
#include <cstring>
#include <iterator>
#include <limits>
#include <memory>
#include <numeric>
#include <utility>

#include <bind/fuchsia/sysmem/heap/cpp/bind.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <fbl/vector.h>

#include "src/graphics/display/drivers/intel-display/clock/cdclk.h"
#include "src/graphics/display/drivers/intel-display/ddi.h"
#include "src/graphics/display/drivers/intel-display/display-device.h"
#include "src/graphics/display/drivers/intel-display/dp-display.h"
#include "src/graphics/display/drivers/intel-display/dpll.h"
#include "src/graphics/display/drivers/intel-display/fuse-config.h"
#include "src/graphics/display/drivers/intel-display/hardware-common.h"
#include "src/graphics/display/drivers/intel-display/hdmi-display.h"
#include "src/graphics/display/drivers/intel-display/pch-engine.h"
#include "src/graphics/display/drivers/intel-display/pci-ids.h"
#include "src/graphics/display/drivers/intel-display/pipe-manager.h"
#include "src/graphics/display/drivers/intel-display/pipe.h"
#include "src/graphics/display/drivers/intel-display/power-controller.h"
#include "src/graphics/display/drivers/intel-display/power.h"
#include "src/graphics/display/drivers/intel-display/registers-ddi.h"
#include "src/graphics/display/drivers/intel-display/registers-dpll.h"
#include "src/graphics/display/drivers/intel-display/registers-pipe-scaler.h"
#include "src/graphics/display/drivers/intel-display/registers-pipe.h"
#include "src/graphics/display/drivers/intel-display/registers.h"
#include "src/graphics/display/drivers/intel-display/tiling.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"
#include "src/graphics/display/lib/driver-utils/poll-until.h"
#include "src/lib/fxl/strings/string_printf.h"

namespace intel_display {

namespace {

constexpr uint32_t kImageTilingTypes[4] = {
    IMAGE_TILING_TYPE_LINEAR,
    IMAGE_TILING_TYPE_X_TILED,
    IMAGE_TILING_TYPE_Y_LEGACY_TILED,
    IMAGE_TILING_TYPE_YF_TILED,
};

constexpr fuchsia_images2::wire::PixelFormat kPixelFormatTypes[2] = {
    fuchsia_images2::wire::PixelFormat::kB8G8R8A8,
    fuchsia_images2::wire::PixelFormat::kR8G8B8A8,
};

// TODO(https://fxbug.dev/42166519): Remove after YUV buffers can be imported to Intel display.
constexpr fuchsia_images2::wire::PixelFormat kYuvPixelFormatTypes[2] = {
    fuchsia_images2::wire::PixelFormat::kI420,
    fuchsia_images2::wire::PixelFormat::kNv12,
};

const display_config_t* FindBanjoConfig(display::DisplayId display_id,
                                        cpp20::span<const display_config_t> banjo_display_configs) {
  auto it =
      std::find_if(banjo_display_configs.begin(), banjo_display_configs.end(),
                   [display_id](const display_config_t& banjo_display_config) {
                     return display::ToDisplayId(banjo_display_config.display_id) == display_id;
                   });
  if (it == banjo_display_configs.end()) {
    return nullptr;
  }
  const display_config_t& config = *it;
  return &config;
}

void GetPostTransformWidth(const layer_t& layer, uint32_t* width, uint32_t* height) {
  const primary_layer_t* primary = &layer.cfg.primary;
  if (primary->image_source_transformation == COORDINATE_TRANSFORMATION_IDENTITY ||
      primary->image_source_transformation == COORDINATE_TRANSFORMATION_ROTATE_CCW_180 ||
      primary->image_source_transformation == COORDINATE_TRANSFORMATION_REFLECT_X ||
      primary->image_source_transformation == COORDINATE_TRANSFORMATION_REFLECT_Y) {
    *width = primary->image_source.width;
    *height = primary->image_source.height;
  } else {
    *width = primary->image_source.height;
    *height = primary->image_source.width;
  }
}

struct FramebufferInfo {
  uint32_t size;
  uint32_t width;
  uint32_t height;
  uint32_t stride;
  zbi_pixel_format_t format;
  int bytes_per_pixel;
};

// The bootloader (UEFI and Depthcharge) informs zircon of the framebuffer information using a
// ZBI_TYPE_FRAMEBUFFER entry. We assume this information to be valid and unmodified by an
// unauthorized call to zx_framebuffer_set_range(), however this is potentially an issue.
// See https://fxbug.dev/42157524.
zx::result<FramebufferInfo> GetFramebufferInfo(const zx::unowned_resource& framebuffer_resource) {
  FramebufferInfo info;
  zx_status_t status = zx_framebuffer_get_info(framebuffer_resource->get(), &info.format,
                                               &info.width, &info.height, &info.stride);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  info.bytes_per_pixel = zbitl::BytesPerPixel(info.format);
  info.size = info.stride * info.height * info.bytes_per_pixel;
  return zx::ok(info);
}

}  // namespace

void Controller::HandleHotplug(DdiId ddi_id, bool long_pulse) {
  FDF_LOG(TRACE, "Hotplug detected on ddi %d (long_pulse=%d)", ddi_id, long_pulse);
  fbl::AutoLock lock(&display_lock_);

  std::unique_ptr<DisplayDevice> device = nullptr;
  for (size_t i = 0; i < display_devices_.size(); i++) {
    if (display_devices_[i]->ddi_id() == ddi_id) {
      if (display_devices_[i]->HandleHotplug(long_pulse)) {
        FDF_LOG(DEBUG, "hotplug handled by device");
        return;
      }
      device = display_devices_.erase(i);
      break;
    }
  }

  // An existing display device was unplugged.
  if (device != nullptr) {
    FDF_LOG(INFO, "Display %" PRIu64 " unplugged", device->id().value());
    display::DisplayId removed_display_id = device->id();
    RemoveDisplay(std::move(device));

    if (engine_listener_.is_valid()) {
      engine_listener_.OnDisplayRemoved(display::ToBanjoDisplayId(removed_display_id));
    }
    return;
  }

  // A new display device was plugged in.
  std::unique_ptr<DisplayDevice> new_device = QueryDisplay(ddi_id, next_id_);
  if (!new_device || !new_device->Init()) {
    FDF_LOG(ERROR, "Failed to initialize hotplug display");
    return;
  }

  DisplayDevice* new_device_ptr = new_device.get();
  zx_status_t add_display_status = AddDisplay(std::move(new_device));
  if (add_display_status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to add a new display: %s", zx_status_get_string(add_display_status));
    return;
  }

  if (engine_listener_.is_valid()) {
    const raw_display_info_t banjo_display_info = new_device_ptr->CreateRawDisplayInfo();
    engine_listener_.OnDisplayAdded(&banjo_display_info);
  }
}

void Controller::HandlePipeVsync(PipeId pipe_id, zx_time_t timestamp) {
  fbl::AutoLock lock(&display_lock_);

  if (!engine_listener_.is_valid()) {
    return;
  }

  display::DisplayId pipe_attached_display_id = display::kInvalidDisplayId;

  display::ConfigStamp vsync_config_stamp = display::kInvalidConfigStamp;

  Pipe* pipe = (*pipe_manager_)[pipe_id];
  if (pipe && pipe->in_use()) {
    pipe_attached_display_id = pipe->attached_display_id();

    registers::PipeRegs regs(pipe_id);
    std::vector<uint64_t> handles;
    for (int i = 0; i < 3; i++) {
      auto live_surface = regs.PlaneSurfaceLive(i).ReadFrom(mmio_space());
      uint64_t handle = live_surface.surface_base_addr() << live_surface.kPageShift;

      if (handle) {
        handles.push_back(handle);
      }
    }

    auto live_surface = regs.CursorSurfaceLive().ReadFrom(mmio_space());
    uint64_t handle = live_surface.surface_base_addr() << live_surface.kPageShift;

    if (handle) {
      handles.push_back(handle);
    }

    vsync_config_stamp = pipe->GetVsyncConfigStamp(handles);
  }

  if (pipe_attached_display_id != display::kInvalidDisplayId) {
    const uint64_t banjo_display_id = display::ToBanjoDisplayId(pipe_attached_display_id);
    const config_stamp_t banjo_config_stamp = display::ToBanjoConfigStamp(vsync_config_stamp);
    engine_listener_.OnDisplayVsync(banjo_display_id, timestamp, &banjo_config_stamp);
  }
}

DisplayDevice* Controller::FindDevice(display::DisplayId display_id) {
  for (auto& d : display_devices_) {
    if (d->id() == display_id) {
      return d.get();
    }
  }
  return nullptr;
}

bool Controller::BringUpDisplayEngine(bool resume) {
  // We follow the steps in the PRM section "Mode Set" > "Sequences to
  // Initialize Display" > "Initialize Sequence", with the tweak that we attempt
  // to reuse the setup left in place by the boot firmware.
  //
  // Tiger Lake: IHD-OS-TGL-Vol 12-1.22-Rev2.0 pages 141-142
  // DG1: IHD-OS-DG1-Vol 12-2.21 pages 119-120
  // Kaby Lake: IHD-OS-KBL-Vol 12-1.17 page 112-113
  // Skylake: IHD-OS-SKL-Vol 12-05.16 page 110

  pch_engine_->SetPchResetHandshake(true);
  if (resume) {
    // The PCH clocks must be set during the display engine initialization
    // sequence. The rest of the PCH configuration will be restored later.
    pch_engine_->RestoreClockParameters();
  } else {
    const PchClockParameters pch_clock_parameters = pch_engine_->ClockParameters();
    PchClockParameters fixed_pch_clock_parameters = pch_clock_parameters;
    pch_engine_->FixClockParameters(fixed_pch_clock_parameters);
    if (pch_clock_parameters != fixed_pch_clock_parameters) {
      FDF_LOG(WARNING, "PCH clocking incorrectly configured. Re-configuring.");
    }
    pch_engine_->SetClockParameters(fixed_pch_clock_parameters);
  }

  // Wait for Power Well 0 distribution
  if (!display::PollUntil(
          [&] { return registers::FuseStatus::Get().ReadFrom(mmio_space()).pg0_dist_status(); },
          zx::usec(1), 20)) {
    FDF_LOG(ERROR, "Power Well 0 distribution failed");
    return false;
  }

  // TODO(https://fxbug.dev/42061147): Currently the driver relies on the assumption that
  // PG1 and Misc IO are always enabled by firmware. We should manually ensure
  // them they are enabled here and disable them on driver teardown.

  ZX_DEBUG_ASSERT(power_);
  if (resume) {
    power_->Resume();
  } else {
    cd_clk_power_well_ = power_->GetCdClockPowerWellRef();
  }

  if (is_tgl(device_id_)) {
    auto pwr_well_ctrl = registers::PowerWellControl::Get().ReadFrom(mmio_space());
    pwr_well_ctrl.power_request(1).set(1);
    pwr_well_ctrl.WriteTo(mmio_space());

    if (!display::PollUntil(
            [&] {
              return registers::PowerWellControl::Get().ReadFrom(mmio_space()).power_state(0).get();
            },
            zx::usec(1), 30)) {
      FDF_LOG(ERROR, "Power Well 1 state failed");
      return false;
    }

    if (!display::PollUntil(
            [&] { return registers::FuseStatus::Get().ReadFrom(mmio_space()).pg1_dist_status(); },
            zx::usec(1), 20)) {
      FDF_LOG(ERROR, "Power Well 1 distribution failed");
      return false;
    }

    // Enable cd_clk and set the frequency to minimum.
    cd_clk_ = std::make_unique<CoreDisplayClockTigerLake>(mmio_space());
    // PLL ratio for 38.4MHz: 16 -> CDCLK 307.2 MHz
    if (!cd_clk_->SetFrequency(307'200)) {
      FDF_LOG(ERROR, "Failed to configure CD clock frequency");
      return false;
    }
  } else {
    // Enable CDCLK PLL to 337.5mhz if the BIOS didn't already enable it. If it needs to be
    // something special (i.e. for eDP), assume that the BIOS already enabled it.
    auto lcpll1_control =
        registers::PllEnable::GetForSkylakeDpll(PllId::DPLL_0).ReadFrom(mmio_space());
    if (!lcpll1_control.pll_enabled()) {
      // Configure DPLL0 frequency before enabling it.
      const auto dpll = PllId::DPLL_0;
      auto dpll_control1 = registers::DisplayPllControl1::Get().ReadFrom(mmio_space());
      dpll_control1.set_pll_uses_hdmi_configuration_mode(dpll, false)
          .set_pll_spread_spectrum_clocking_enabled(dpll, false)
          .set_pll_display_port_ddi_frequency_mhz(dpll, 810)
          .set_pll_programming_enabled(dpll, true)
          .WriteTo(mmio_space());

      // Enable DPLL0 and wait for it.
      lcpll1_control.set_pll_enabled(true);
      lcpll1_control.WriteTo(mmio_space());

      // The PRM instructs us to use the LCPLL1 control register to find out
      // when DPLL0 locks. This is different from most DPLL enabling sequences,
      // which use the DPLL status registers.
      if (!display::PollUntil(
              [&] {
                return lcpll1_control.ReadFrom(mmio_space()).pll_locked_tiger_lake_and_lcpll1();
              },
              zx::msec(1), 5)) {
        FDF_LOG(ERROR, "DPLL0 / LCPLL1 did not lock in 5us");
        return false;
      }

      // Enable cd_clk and set the frequency to minimum.
      cd_clk_ = std::make_unique<CoreDisplayClockSkylake>(mmio_space());
      if (!cd_clk_->SetFrequency(337'500)) {
        FDF_LOG(ERROR, "Failed to configure CD clock frequency");
        return false;
      }
    } else {
      cd_clk_ = std::make_unique<CoreDisplayClockSkylake>(mmio_space());
      FDF_LOG(INFO, "CDCLK already assigned by BIOS: frequency: %u KHz",
              cd_clk_->current_freq_khz());
    }
  }

  // Power up DBUF (Data Buffer) slices.
  FDF_LOG(TRACE, "Powering up DBUF (Data Buffer) slices");
  const int display_buffer_slice_count = is_tgl(device_id_) ? 2 : 1;
  for (int slice_index = 0; slice_index < display_buffer_slice_count; ++slice_index) {
    auto display_buffer_control =
        registers::DataBufferControl::GetForSlice(slice_index).ReadFrom(mmio_space());
    display_buffer_control.set_powered_on_target(true).WriteTo(mmio_space());

    if (!display::PollUntil(
            [&] { return display_buffer_control.ReadFrom(mmio_space()).powered_on(); }, zx::usec(1),
            10)) {
      FDF_LOG(ERROR, "DBUF slice %d did not power up in time", slice_index + 1);
      return false;
    }
  }

  // We never use VGA, so just disable it at startup
  constexpr uint16_t kSequencerIdx = 0x3c4;
  constexpr uint16_t kSequencerData = 0x3c5;
  constexpr uint8_t kClockingModeIdx = 1;
  constexpr uint8_t kClockingModeScreenOff = (1 << 5);
  zx_status_t status = zx_ioports_request(resources_.ioport->get(), kSequencerIdx, 2);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map vga ports");
    return false;
  }
  outp(kSequencerIdx, kClockingModeIdx);
  uint8_t clocking_mode = inp(kSequencerData);
  if (!(clocking_mode & kClockingModeScreenOff)) {
    outp(kSequencerIdx, inp(kSequencerData) | kClockingModeScreenOff);
    zx_nanosleep(zx_deadline_after(ZX_MSEC(100)));

    auto vga_ctl = registers::VgaCtl::Get().ReadFrom(mmio_space());
    vga_ctl.set_vga_display_disable(1);
    vga_ctl.WriteTo(mmio_space());
  }

  for (Pipe* pipe : *pipe_manager_) {
    pipe->Reset();
    ResetPipePlaneBuffers(pipe->pipe_id());

    registers::PipeRegs pipe_regs(pipe->pipe_id());

    // Disable the scalers (double buffered on PipeScalerWindowSize), since
    // we don't know what state they are in at boot.
    auto pipe_scaler_0_regs = registers::PipeScalerRegs(pipe->pipe_id(), 0);
    pipe_scaler_0_regs.PipeScalerControlSkylake()
        .ReadFrom(mmio_space())
        .set_is_enabled(0)
        .WriteTo(mmio_space());
    pipe_scaler_0_regs.PipeScalerWindowSize().ReadFrom(mmio_space()).WriteTo(mmio_space());
    if (pipe->pipe_id() != PipeId::PIPE_C) {
      auto pipe_scaler_1_regs = registers::PipeScalerRegs(pipe->pipe_id(), 1);
      pipe_scaler_1_regs.PipeScalerControlSkylake()
          .ReadFrom(mmio_space())
          .set_is_enabled(0)
          .WriteTo(mmio_space());
      pipe_scaler_1_regs.PipeScalerWindowSize().ReadFrom(mmio_space()).WriteTo(mmio_space());
    }

    // Disable the cursor watermark
    for (int wm_num = 0; wm_num < 8; wm_num++) {
      auto wm = pipe_regs.PlaneWatermark(0, wm_num).FromValue(0);
      wm.WriteTo(mmio_space());
    }

    // Disable the primary plane watermarks and reset their buffer allocation
    for (unsigned plane_num = 0; plane_num < registers::kImagePlaneCount; plane_num++) {
      for (int wm_num = 0; wm_num < 8; wm_num++) {
        auto wm = pipe_regs.PlaneWatermark(plane_num + 1, wm_num).FromValue(0);
        wm.WriteTo(mmio_space());
      }
    }
  }

  return true;
}

void Controller::ResetPipePlaneBuffers(PipeId pipe_id) {
  fbl::AutoLock lock(&plane_buffers_lock_);
  const uint16_t data_buffer_block_count = DataBufferBlockCount();
  for (unsigned plane_num = 0; plane_num < registers::kImagePlaneCount; plane_num++) {
    plane_buffers_[pipe_id][plane_num].start = data_buffer_block_count;
  }
}

bool Controller::ResetDdi(DdiId ddi_id, std::optional<TranscoderId> transcoder_id) {
  registers::DdiRegs ddi_regs(ddi_id);

  // Disable the port
  auto ddi_buffer_control = ddi_regs.BufferControl().ReadFrom(mmio_space());
  const bool was_enabled = ddi_buffer_control.enabled();
  ddi_buffer_control.set_enabled(false).WriteTo(mmio_space());

  if (!is_tgl(device_id_)) {
    auto dp_transport_control = ddi_regs.DpTransportControl().ReadFrom(mmio_space());
    dp_transport_control.set_enabled(false)
        .set_training_pattern(registers::DpTransportControl::kTrainingPattern1)
        .WriteTo(mmio_space());
  } else {
    if (transcoder_id.has_value()) {
      auto dp_transport_control =
          registers::DpTransportControl::GetForTigerLakeTranscoder(*transcoder_id)
              .ReadFrom(mmio_space());
      dp_transport_control.set_enabled(false)
          .set_training_pattern(registers::DpTransportControl::kTrainingPattern1)
          .WriteTo(mmio_space());
    }
  }

  if (was_enabled &&
      !display::PollUntil([&] { return ddi_buffer_control.ReadFrom(mmio_space()).is_idle(); },
                          zx::msec(1), 8)) {
    FDF_LOG(ERROR, "Port failed to go idle");
    return false;
  }

  // Disable IO power
  ZX_DEBUG_ASSERT(power_);
  power_->SetDdiIoPowerState(ddi_id, /* enable */ false);

  // Wait for DDI IO power to be fully disabled.
  // This step is not documented in Intel Display PRM, but this step occurs
  // in the drm/i915 driver and experiments on NUC11 hardware indicate that
  // display hotplug may fail without this step.
  if (!display::PollUntil([&] { return !power_->GetDdiIoPowerState(ddi_id); }, zx::usec(1), 1000)) {
    FDF_LOG(ERROR, "Disable IO power timeout");
    return false;
  }

  if (!dpll_manager_->ResetDdiPll(ddi_id)) {
    FDF_LOG(ERROR, "Failed to unmap DPLL for DDI %d", ddi_id);
    return false;
  }

  return true;
}

zx_status_t Controller::InitGttForTesting(const ddk::Pci& pci, fdf::MmioBuffer buffer,
                                          uint32_t fb_offset) {
  fbl::AutoLock gtt_lock(&gtt_lock_);
  return gtt_.Init(pci, std::move(buffer), fb_offset);
}

const GttRegion& Controller::SetupGttImage(const image_metadata_t& image_metadata,
                                           uint64_t image_handle, uint32_t rotation) {
  const std::unique_ptr<GttRegionImpl>& region = GetGttRegionImpl(image_handle);
  ZX_DEBUG_ASSERT(region);
  region->SetRotation(rotation, image_metadata);
  return *region;
}

std::unique_ptr<DisplayDevice> Controller::QueryDisplay(DdiId ddi_id,
                                                        display::DisplayId display_id) {
  fbl::AllocChecker ac;
  if (!igd_opregion_.HasDdi(ddi_id)) {
    FDF_LOG(INFO, "ddi %d not available.", ddi_id);
    return nullptr;
  }

  if (igd_opregion_.SupportsDp(ddi_id)) {
    FDF_LOG(DEBUG, "Checking for DisplayPort monitor at DDI %d", ddi_id);
    DdiReference ddi_reference_maybe = ddi_manager_->GetDdiReference(ddi_id);
    if (!ddi_reference_maybe) {
      FDF_LOG(DEBUG, "DDI %d PHY not available. Skip querying.", ddi_id);
    } else {
      auto dp_disp = fbl::make_unique_checked<DpDisplay>(
          &ac, this, display_id, ddi_id, &dp_auxs_[ddi_id], &pch_engine_.value(),
          std::move(ddi_reference_maybe), &root_node_);
      if (ac.check() && reinterpret_cast<DisplayDevice*>(dp_disp.get())->Query()) {
        return dp_disp;
      }
    }
  }
  if (igd_opregion_.SupportsHdmi(ddi_id) || igd_opregion_.SupportsDvi(ddi_id)) {
    FDF_LOG(DEBUG, "Checking for HDMI monitor at DDI %d", ddi_id);
    DdiReference ddi_reference_maybe = ddi_manager_->GetDdiReference(ddi_id);
    if (!ddi_reference_maybe) {
      FDF_LOG(DEBUG, "DDI %d PHY not available. Skip querying.", ddi_id);
    } else {
      auto hdmi_disp = fbl::make_unique_checked<HdmiDisplay>(
          &ac, this, display_id, ddi_id, std::move(ddi_reference_maybe), gmbus_i2cs_[ddi_id].i2c());
      if (ac.check() && reinterpret_cast<DisplayDevice*>(hdmi_disp.get())->Query()) {
        return hdmi_disp;
      }
    }
  }
  FDF_LOG(TRACE, "Nothing found for ddi %d!", ddi_id);
  return nullptr;
}

bool Controller::LoadHardwareState(DdiId ddi_id, DisplayDevice* device) {
  registers::DdiRegs regs(ddi_id);

  if (!power_->GetDdiIoPowerState(ddi_id) ||
      !regs.BufferControl().ReadFrom(mmio_space()).enabled()) {
    return false;
  }

  DdiPllConfig pll_config = dpll_manager()->LoadState(ddi_id);
  if (pll_config.IsEmpty()) {
    FDF_LOG(ERROR, "Cannot load DPLL state for DDI %d", ddi_id);
    return false;
  }

  bool init_result = device->InitWithDdiPllConfig(pll_config);
  if (!init_result) {
    FDF_LOG(ERROR, "Cannot initialize the display with DPLL state for DDI %d", ddi_id);
    return false;
  }

  device->LoadActiveMode();
  return true;
}

void Controller::InitDisplays() {
  fbl::AutoLock lock(&display_lock_);
  BringUpDisplayEngine(false);

  if (!ReadMemoryLatencyInfo()) {
    return;
  }

  // This disables System Agent Geyserville (SAGV), which dynamically adjusts
  // the system agent voltage and clock frequencies depending on system power
  // and performance requirements.
  //
  // When SAGV is enabled, it could limit the display memory bandwidth (on Tiger
  // Lake+) and block the display engine from accessing system memory for a
  // certain amount of time (SAGV block time). Thus, SAGV must be disabled if
  // the display engine's memory latency exceeds the SAGV block time.
  //
  // Here, we unconditionally disable SAGV to guarantee the correctness of
  // the display engine memory accesses. However, this may cause the processor
  // to consume more power, even to the point of exceeding its thermal envelope.
  DisableSystemAgentGeyserville();

  for (const auto ddi_id : ddis_) {
    auto disp_device = QueryDisplay(ddi_id, next_id_);
    if (disp_device) {
      AddDisplay(std::move(disp_device));
    }
  }

  if (display_devices_.size() == 0) {
    FDF_LOG(INFO, "intel-display: No displays detected.");
  }

  // Make a note of what needs to be reset, so we can finish querying the hardware state
  // before touching it, and so we can make sure transcoders are reset before ddis.
  std::vector<std::pair<DdiId, std::optional<TranscoderId>>> ddi_trans_needs_reset;
  std::vector<DisplayDevice*> device_needs_init;

  for (const auto ddi_id : ddis_) {
    DisplayDevice* device = nullptr;
    for (auto& display_device : display_devices_) {
      if (display_device->ddi_id() == ddi_id) {
        device = display_device.get();
        break;
      }
    }

    if (device == nullptr) {
      ddi_trans_needs_reset.emplace_back(ddi_id, std::nullopt);
    } else {
      if (!LoadHardwareState(ddi_id, device)) {
        auto transcoder_maybe = device->pipe()
                                    ? std::make_optional(device->pipe()->connected_transcoder_id())
                                    : std::nullopt;
        ddi_trans_needs_reset.emplace_back(ddi_id, transcoder_maybe);
        device_needs_init.push_back(device);
      } else {
        // On Tiger Lake, if a display device is already initialized by BIOS,
        // the pipe / transcoder / DDI should be all reset and reinitialized.
        // By doing this we can keep the display state fully controlled by the
        // driver.
        // TODO(https://fxbug.dev/42063039): Consider doing this on all platforms.
        if (is_tgl(device_id())) {
          device_needs_init.push_back(device);
        }
        device->InitBacklight();
      }
    }
  }

  // Reset any transcoders which aren't in use
  pipe_manager_->ResetInactiveTranscoders();

  // Reset any ddis which don't have a restored display. If we failed to restore a
  // display, try to initialize it here.
  for (const auto& [ddi, transcoder_maybe] : ddi_trans_needs_reset) {
    ResetDdi(ddi, transcoder_maybe);
  }

  for (DisplayDevice* device : device_needs_init) {
    ZX_ASSERT_MSG(device, "device_needs_init incorrectly populated above");
    for (unsigned i = 0; i < display_devices_.size(); i++) {
      if (display_devices_[i].get() == device) {
        if (is_tgl(device_id())) {
          // On Tiger Lake, devices pre-initialized by the BIOS must be reset
          // and reinitialized by the driver.
          // TODO(https://fxbug.dev/42063040): We should fix the device reset logic so
          // that we don't need to delete the old device.
          const DdiId ddi_id = device->ddi_id();
          const display::DisplayId display_id = device->id();
          display_devices_[i].reset();
          display_devices_[i] = QueryDisplay(ddi_id, display_id);
          device = display_devices_[i].get();
          if (!device || !device->Init()) {
            display_devices_.erase(i);
          }
        } else {
          if (!device->Init()) {
            display_devices_.erase(i);
          }
        }
        break;
      }
    }
  }
}

bool Controller::ReadMemoryLatencyInfo() {
  PowerController power_controller(&*mmio_space_);

  const zx::result<std::array<uint8_t, 8>> memory_latency =
      power_controller.GetRawMemoryLatencyDataUs();
  if (memory_latency.is_error()) {
    // We're not supposed to enable planes if we can't read the memory latency
    // data. This makes the display driver fairly useless, so bail.
    FDF_LOG(ERROR, "Error reading memory latency data from PCU firmware: %s",
            memory_latency.status_string());
    return false;
  }
  FDF_LOG(TRACE, "Raw PCU memory latency data: %u %u %u %u %u %u %u %u", memory_latency.value()[0],
          memory_latency.value()[1], memory_latency.value()[2], memory_latency.value()[3],
          memory_latency.value()[4], memory_latency.value()[5], memory_latency.value()[6],
          memory_latency.value()[7]);

  // Pre-Tiger Lake, the SAGV blocking time is always modeled to 30us.
  const zx::result<uint32_t> blocking_time =
      is_tgl(device_id_) ? power_controller.GetSystemAgentBlockTimeUsTigerLake()
                         : power_controller.GetSystemAgentBlockTimeUsKabyLake();
  if (blocking_time.is_error()) {
    // We're not supposed to enable planes if we can't read the SAGV blocking
    // time. This makes the display driver fairly useless, so bail.
    FDF_LOG(ERROR, "Error reading SAGV blocking time from PCU firmware: %s",
            blocking_time.status_string());
    return false;
  }
  FDF_LOG(TRACE, "System Agent Geyserville blocking time: %u", blocking_time.value());

  // The query below is only supported on Tiger Lake PCU firmware.
  if (!is_tgl(device_id_)) {
    return true;
  }

  const zx::result<MemorySubsystemInfo> memory_info =
      power_controller.GetMemorySubsystemInfoTigerLake();
  if (memory_info.is_error()) {
    // We can handle this error by unconditionally disabling SAGV.
    FDF_LOG(ERROR, "Error reading SAGV QGV point info from PCU firmware: %s",
            blocking_time.status_string());
    return true;
  }

  const MemorySubsystemInfo::GlobalInfo& global_info = memory_info.value().global_info;
  FDF_LOG(TRACE, "PCU memory subsystem info: DRAM type %d, %d channels, %d SAGV points",
          static_cast<int>(global_info.ram_type), global_info.memory_channel_count,
          global_info.agent_point_count);
  for (int point_index = 0; point_index < global_info.agent_point_count; ++point_index) {
    const MemorySubsystemInfo::AgentPoint& point_info = memory_info.value().points[point_index];
    FDF_LOG(TRACE, "SAGV point %d info: DRAM clock %d kHz, tRP %d, tRCD %d, tRDPRE %d, tRAS %d",
            point_index, point_info.dram_clock_khz, point_info.row_precharge_to_open_cycles,
            point_info.row_access_to_column_access_delay_cycles,
            point_info.read_to_precharge_cycles, point_info.row_activate_to_precharge_cycles);
  }
  return true;
}

void Controller::DisableSystemAgentGeyserville() {
  PowerController power_controller(&*mmio_space_);

  const zx::result<> sagv_disabled = power_controller.SetSystemAgentGeyservilleEnabled(
      false, PowerController::RetryBehavior::kRetryUntilStateChanges);
  if (sagv_disabled.is_error()) {
    FDF_LOG(ERROR, "Failed to disable System Agent Geyserville. Display corruption may occur.");
    return;
  }
  FDF_LOG(TRACE, "System Agent Geyserville disabled.");
}

void Controller::RemoveDisplay(std::unique_ptr<DisplayDevice> display) {
  // Make sure the display's resources get freed before reallocating the pipe buffers by letting
  // "display" go out of scope.
}

zx_status_t Controller::AddDisplay(std::unique_ptr<DisplayDevice> display) {
  const display::DisplayId display_id = display->id();

  // Add the new device.
  fbl::AllocChecker ac;
  display_devices_.push_back(std::move(display), &ac);
  if (!ac.check()) {
    FDF_LOG(WARNING, "Failed to add display device");
    return ZX_ERR_NO_MEMORY;
  }

  FDF_LOG(INFO, "Display %" PRIu64 " connected", display_id.value());
  next_id_++;
  return ZX_OK;
}

// DisplayEngine methods

void Controller::DisplayEngineRegisterDisplayEngineListener(
    const display_engine_listener_protocol_t* engine_listener) {
  fbl::AutoLock lock(&display_lock_);
  engine_listener_ = ddk::DisplayEngineListenerProtocolClient(engine_listener);

  // If `RegisterDisplayEngineListener` occurs **after** driver initialization
  // (i.e. `driver_initialized_` is true), `RegisterDisplayEngineListener`
  // should be responsible for notifying the coordinator of existing display
  // devices.
  //
  // Otherwise, the driver initialization logic (`DdkInit()`) should be
  // responsible for notifying the coordinator of existing display devices.
  if (driver_initialized_ && !display_devices_.is_empty()) {
    for (const std::unique_ptr<DisplayDevice>& display_device : display_devices_) {
      const raw_display_info_t banjo_display_info = display_device->CreateRawDisplayInfo();
      engine_listener_.OnDisplayAdded(&banjo_display_info);
    }
  }
}

void Controller::DisplayEngineDeregisterDisplayEngineListener() {
  fbl::AutoLock lock(&display_lock_);
  engine_listener_ = ddk::DisplayEngineListenerProtocolClient();
}

static bool ConvertPixelFormatToTilingType(
    fuchsia_sysmem2::wire::ImageFormatConstraints constraints, uint32_t* image_tiling_type_out) {
  const auto& format = constraints.pixel_format();
  if (format != fuchsia_images2::wire::PixelFormat::kB8G8R8A8 &&
      format != fuchsia_images2::wire::PixelFormat::kR8G8B8A8) {
    return false;
  }

  if (!constraints.has_pixel_format_modifier()) {
    return false;
  }

  switch (constraints.pixel_format_modifier()) {
    case fuchsia_images2::wire::PixelFormatModifier::kIntelI915XTiled:
      *image_tiling_type_out = IMAGE_TILING_TYPE_X_TILED;
      return true;

    case fuchsia_images2::wire::PixelFormatModifier::kIntelI915YTiled:
      *image_tiling_type_out = IMAGE_TILING_TYPE_Y_LEGACY_TILED;
      return true;

    case fuchsia_images2::wire::PixelFormatModifier::kIntelI915YfTiled:
      *image_tiling_type_out = IMAGE_TILING_TYPE_YF_TILED;
      return true;

    case fuchsia_images2::wire::PixelFormatModifier::kLinear:
      *image_tiling_type_out = IMAGE_TILING_TYPE_LINEAR;
      return true;

    default:
      return false;
  }
}

zx_status_t Controller::DisplayEngineImportBufferCollection(
    uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token) {
  display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) != buffer_collections_.end()) {
    FDF_LOG(ERROR, "Buffer Collection (id=%lu) already exists",
            driver_buffer_collection_id.value());
    return ZX_ERR_ALREADY_EXISTS;
  }

  ZX_DEBUG_ASSERT_MSG(sysmem_.is_valid(), "sysmem allocator is not initialized");

  auto [collection_client_endpoint, collection_server_endpoint] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollection>::Create();

  fidl::Arena arena;
  auto bind_result = sysmem_->BindSharedCollection(
      fuchsia_sysmem2::wire::AllocatorBindSharedCollectionRequest::Builder(arena)
          .buffer_collection_request(std::move(collection_server_endpoint))
          .token(
              fidl::ClientEnd<fuchsia_sysmem2::BufferCollectionToken>(std::move(collection_token)))
          .Build());
  if (!bind_result.ok()) {
    FDF_LOG(ERROR, "Cannot complete FIDL call BindSharedCollection: %s",
            bind_result.status_string());
    return ZX_ERR_INTERNAL;
  }

  buffer_collections_[driver_buffer_collection_id] =
      fidl::WireSyncClient(std::move(collection_client_endpoint));
  return ZX_OK;
}

zx_status_t Controller::DisplayEngineReleaseBufferCollection(
    uint64_t banjo_driver_buffer_collection_id) {
  display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  if (buffer_collections_.find(driver_buffer_collection_id) == buffer_collections_.end()) {
    FDF_LOG(ERROR, "Cannot release buffer collection %lu: buffer collection doesn't exist",
            driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  buffer_collections_.erase(driver_buffer_collection_id);
  return ZX_OK;
}

zx_status_t Controller::DisplayEngineImportImage(const image_metadata_t* image_metadata,
                                                 uint64_t banjo_driver_buffer_collection_id,
                                                 uint32_t index, uint64_t* out_image_handle) {
  display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    FDF_LOG(ERROR, "ImportImage: Cannot find imported buffer collection (id=%lu)",
            driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  const fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>& collection = it->second;

  if (!(image_metadata->tiling_type == IMAGE_TILING_TYPE_LINEAR ||
        image_metadata->tiling_type == IMAGE_TILING_TYPE_X_TILED ||
        image_metadata->tiling_type == IMAGE_TILING_TYPE_Y_LEGACY_TILED ||
        image_metadata->tiling_type == IMAGE_TILING_TYPE_YF_TILED)) {
    return ZX_ERR_INVALID_ARGS;
  }

  fidl::WireResult check_result = collection->CheckAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!check_result.ok()) {
    FDF_LOG(ERROR, "Failed to check buffers allocated, %s",
            check_result.FormatDescription().c_str());
    return check_result.status();
  }
  const auto& check_response = check_result.value();
  if (check_response.is_error()) {
    if (check_response.error_value() == fuchsia_sysmem2::wire::Error::kPending) {
      return ZX_ERR_SHOULD_WAIT;
    }
    return sysmem::V1CopyFromV2Error(check_response.error_value());
  }

  fidl::WireResult wait_result = collection->WaitForAllBuffersAllocated();
  // TODO(https://fxbug.dev/42072690): The sysmem FIDL error logging patterns are
  // inconsistent across drivers. The FIDL error handling and logging should be
  // unified.
  if (!wait_result.ok()) {
    FDF_LOG(ERROR, "Failed to wait for buffers allocated, %s",
            wait_result.FormatDescription().c_str());
    return wait_result.status();
  }
  const auto& wait_response = wait_result.value();
  if (wait_response.is_error()) {
    if (wait_response.error_value() == fuchsia_sysmem2::wire::Error::kPending) {
      return ZX_ERR_SHOULD_WAIT;
    }
    return sysmem::V1CopyFromV2Error(wait_response.error_value());
  }
  const auto& wait_value = wait_response.value();
  fuchsia_sysmem2::wire::BufferCollectionInfo& collection_info =
      wait_value->buffer_collection_info();

  if (!collection_info.settings().has_image_format_constraints()) {
    FDF_LOG(ERROR, "No image format constraints");
    return ZX_ERR_INVALID_ARGS;
  }
  if (index >= collection_info.buffers().count()) {
    FDF_LOG(ERROR, "Invalid index %d greater than buffer count %lu", index,
            collection_info.buffers().count());
    return ZX_ERR_OUT_OF_RANGE;
  }

  zx::vmo vmo = std::move(collection_info.buffers().at(index).vmo());

  uint64_t offset = collection_info.buffers().at(index).vmo_usable_start();
  if (offset % PAGE_SIZE != 0) {
    FDF_LOG(ERROR, "Invalid offset");
    return ZX_ERR_INVALID_ARGS;
  }

  ZX_DEBUG_ASSERT(collection_info.settings().image_format_constraints().pixel_format() !=
                      fuchsia_images2::wire::PixelFormat::kI420 &&
                  collection_info.settings().image_format_constraints().pixel_format() !=
                      fuchsia_images2::wire::PixelFormat::kNv12);
  uint32_t image_tiling_type;
  if (!ConvertPixelFormatToTilingType(collection_info.settings().image_format_constraints(),
                                      &image_tiling_type)) {
    FDF_LOG(ERROR, "Invalid pixel format modifier");
    return ZX_ERR_INVALID_ARGS;
  }
  if (image_metadata->tiling_type != image_tiling_type) {
    FDF_LOG(ERROR, "Incompatible image type from image %d and sysmem %d",
            image_metadata->tiling_type, image_tiling_type);
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AutoLock lock(&gtt_lock_);
  fbl::AllocChecker ac;
  imported_images_.reserve(imported_images_.size() + 1, &ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  fidl::Arena arena;
  auto format =
      ImageConstraintsToFormat(arena, collection_info.settings().image_format_constraints(),
                               image_metadata->width, image_metadata->height);
  if (!format.is_ok()) {
    FDF_LOG(ERROR, "Failed to get format from constraints");
    return ZX_ERR_INVALID_ARGS;
  }

  const uint32_t length = [&]() {
    const uint64_t length = ImageFormatImageSize(format.value());
    ZX_DEBUG_ASSERT_MSG(length <= std::numeric_limits<uint32_t>::max(), "%lu overflows uint32_t",
                        length);
    return static_cast<uint32_t>(length);
  }();

  const uint32_t bytes_per_pixel =
      ImageFormatStrideBytesPerWidthPixel(PixelFormatAndModifierFromImageFormat(format.value()));

  ZX_DEBUG_ASSERT(
      length >=
      width_in_tiles(image_metadata->tiling_type, image_metadata->width, bytes_per_pixel) *
          height_in_tiles(image_metadata->tiling_type, image_metadata->height) *
          get_tile_byte_size(image_metadata->tiling_type));

  uint32_t align;
  if (image_metadata->tiling_type == IMAGE_TILING_TYPE_LINEAR) {
    align = registers::PlaneSurface::kLinearAlignment;
  } else if (image_metadata->tiling_type == IMAGE_TILING_TYPE_X_TILED) {
    align = registers::PlaneSurface::kXTilingAlignment;
  } else {
    align = registers::PlaneSurface::kYTilingAlignment;
  }
  std::unique_ptr<GttRegionImpl> gtt_region;
  zx_status_t status = gtt_.AllocRegion(length, align, &gtt_region);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to allocate GTT region, status %s", zx_status_get_string(status));
    return status;
  }

  // The vsync logic requires that images not have base == 0
  if (gtt_region->base() == 0) {
    std::unique_ptr<GttRegionImpl> alt_gtt_region;
    zx_status_t status = gtt_.AllocRegion(length, align, &alt_gtt_region);
    if (status != ZX_OK) {
      return status;
    }
    gtt_region = std::move(alt_gtt_region);
  }

  status = gtt_region->PopulateRegion(vmo.release(), offset / PAGE_SIZE, length);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to populate GTT region, status %s", zx_status_get_string(status));
    return status;
  }

  gtt_region->set_bytes_per_row(format.value().bytes_per_row());
  const display::DriverImageId image_id(gtt_region->base());
  imported_images_.push_back(std::move(gtt_region));

  ZX_DEBUG_ASSERT_MSG(
      imported_image_pixel_formats_.find(image_id) == imported_image_pixel_formats_.end(),
      "Image ID %" PRIu64 " exists in imported image pixel formats map", image_id.value());
  imported_image_pixel_formats_.emplace(image_id,
                                        PixelFormatAndModifierFromImageFormat(format.value()));

  *out_image_handle = display::ToBanjoDriverImageId(image_id);
  return ZX_OK;
}

void Controller::DisplayEngineReleaseImage(uint64_t image_handle) {
  const uint64_t gtt_region_base = image_handle;
  const display::DriverImageId image_id(gtt_region_base);

  fbl::AutoLock lock(&gtt_lock_);
  imported_image_pixel_formats_.erase(image_id);
  for (unsigned i = 0; i < imported_images_.size(); i++) {
    if (imported_images_[i]->base() == gtt_region_base) {
      imported_images_[i]->ClearRegion();
      imported_images_.erase(i);
      return;
    }
  }
}

PixelFormatAndModifier Controller::GetImportedImagePixelFormat(
    display::DriverImageId image_id) const {
  fbl::AutoLock lock(&gtt_lock_);
  auto it = imported_image_pixel_formats_.find(image_id);
  if (it != imported_image_pixel_formats_.end()) {
    return it->second;
  }
  ZX_ASSERT_MSG(false, "Imported image ID %" PRIu64 " not found", image_id.value());
}

const std::unique_ptr<GttRegionImpl>& Controller::GetGttRegionImpl(uint64_t handle) {
  fbl::AutoLock lock(&gtt_lock_);
  for (auto& region : imported_images_) {
    if (region->base() == handle) {
      return region;
    }
  }
  ZX_ASSERT(false);
}

bool Controller::GetPlaneLayer(Pipe* pipe, uint32_t plane,
                               cpp20::span<const display_config_t> banjo_display_configs,
                               const layer_t** layer_out) {
  if (!pipe->in_use()) {
    return false;
  }
  display::DisplayId pipe_attached_display_id = pipe->attached_display_id();

  for (const display_config_t& banjo_display_config : banjo_display_configs) {
    display::DisplayId display_id = display::ToDisplayId(banjo_display_config.display_id);
    if (display_id != pipe_attached_display_id) {
      continue;
    }
    bool has_color_layer = banjo_display_config.layer_count &&
                           banjo_display_config.layer_list[0].type == LAYER_TYPE_COLOR;
    for (unsigned layer_index = 0; layer_index < banjo_display_config.layer_count; ++layer_index) {
      const layer_t& layer = banjo_display_config.layer_list[layer_index];
      if (layer.type == LAYER_TYPE_PRIMARY) {
        if (plane + (has_color_layer ? 1 : 0) != layer_index) {
          continue;
        }
      } else if (layer.type == LAYER_TYPE_COLOR) {
        // color layers aren't a plane
        continue;
      } else {
        ZX_ASSERT(false);
      }
      *layer_out = &banjo_display_config.layer_list[layer_index];
      return true;
    }
  }
  return false;
}

uint16_t Controller::CalculateBuffersPerPipe(size_t active_pipe_count) {
  ZX_ASSERT(active_pipe_count < PipeIds<registers::Platform::kKabyLake>().size());
  return DataBufferBlockCount() / active_pipe_count;
}

bool Controller::CalculateMinimumAllocations(
    cpp20::span<const display_config_t> banjo_display_configs,
    uint16_t min_allocs[PipeIds<registers::Platform::kKabyLake>().size()]
                       [registers::kImagePlaneCount]) {
  // This fn ignores layers after kImagePlaneCount. Displays with too many layers already
  // failed in ::CheckConfiguration, so it doesn't matter if we incorrectly say they pass here.
  bool success = true;
  for (Pipe* pipe : *pipe_manager_) {
    PipeId pipe_id = pipe->pipe_id();
    uint32_t total = 0;

    for (unsigned plane_num = 0; plane_num < registers::kImagePlaneCount; plane_num++) {
      const layer_t* layer;
      if (!GetPlaneLayer(pipe, plane_num, banjo_display_configs, &layer)) {
        min_allocs[pipe_id][plane_num] = 0;
        continue;
      }

      ZX_ASSERT(layer->type == LAYER_TYPE_PRIMARY);
      const primary_layer_t* primary = &layer->cfg.primary;

      if (primary->image_metadata.tiling_type == IMAGE_TILING_TYPE_LINEAR ||
          primary->image_metadata.tiling_type == IMAGE_TILING_TYPE_X_TILED) {
        min_allocs[pipe_id][plane_num] = 8;
      } else {
        uint32_t plane_source_width;
        uint32_t min_scan_lines;

        // TODO(https://fxbug.dev/42076788): Currently we assume only RGBA/BGRA formats
        // are supported and hardcode the bytes-per-pixel value to avoid pixel
        // format check and stride calculation (which requires holding the GTT
        // lock). This may change when we need to support non-RGBA/BGRA images.
        //
        // There is currently no good way to enforce this by assertions,
        // because the image handle provided in `banjo_display_configs` can be
        // invalid or obsolete when `CheckConfiguration()` calls this method.
        static constexpr int bytes_per_pixel = 4;

        if (primary->image_source_transformation == COORDINATE_TRANSFORMATION_IDENTITY ||
            primary->image_source_transformation == COORDINATE_TRANSFORMATION_ROTATE_CCW_180) {
          plane_source_width = primary->image_source.width;
          min_scan_lines = 8;
        } else {
          plane_source_width = primary->image_source.height;
          min_scan_lines = 32 / bytes_per_pixel;
        }
        min_allocs[pipe_id][plane_num] = static_cast<uint16_t>(
            ((fbl::round_up(4u * plane_source_width * bytes_per_pixel, 512u) / 512u) *
             (min_scan_lines / 4)) +
            3);
        if (min_allocs[pipe_id][plane_num] < 8) {
          min_allocs[pipe_id][plane_num] = 8;
        }
      }
      total += min_allocs[pipe_id][plane_num];
    }

    if (total && total > CalculateBuffersPerPipe(banjo_display_configs.size())) {
      min_allocs[pipe_id][0] = UINT16_MAX;
      success = false;
    }
  }

  return success;
}

void Controller::UpdateAllocations(
    const uint16_t min_allocs[PipeIds<registers::Platform::kKabyLake>().size()]
                             [registers::kImagePlaneCount],
    const uint64_t data_rate_bytes_per_frame[PipeIds<registers::Platform::kKabyLake>().size()]
                                            [registers::kImagePlaneCount]) {
  uint16_t allocs[PipeIds<registers::Platform::kKabyLake>().size()][registers::kImagePlaneCount];

  for (unsigned pipe_num = 0; pipe_num < PipeIds<registers::Platform::kKabyLake>().size();
       pipe_num++) {
    uint64_t total_data_rate_bytes_per_frame = 0;
    for (unsigned plane_num = 0; plane_num < registers::kImagePlaneCount; plane_num++) {
      total_data_rate_bytes_per_frame += data_rate_bytes_per_frame[pipe_num][plane_num];
    }
    if (total_data_rate_bytes_per_frame == 0) {
      for (unsigned plane_num = 0; plane_num < registers::kImagePlaneCount; plane_num++) {
        allocs[pipe_num][plane_num] = 0;
      }
      continue;
    }

    // Allocate buffers based on the percentage of the total pixel bandwidth they take. If
    // that percentage isn't enough for a plane, give that plane its minimum allocation and
    // then try again.
    double buffers_per_pipe = pipe_buffers_[pipe_num].end - pipe_buffers_[pipe_num].start;
    bool forced_alloc[registers::kImagePlaneCount] = {};
    bool done = false;
    while (!done) {
      for (unsigned plane_num = 0; plane_num < registers::kImagePlaneCount; plane_num++) {
        if (forced_alloc[plane_num]) {
          continue;
        }

        double blocks = buffers_per_pipe *
                        static_cast<double>(data_rate_bytes_per_frame[pipe_num][plane_num]) /
                        static_cast<double>(total_data_rate_bytes_per_frame);
        allocs[pipe_num][plane_num] = static_cast<uint16_t>(blocks);
      }

      done = true;

      for (unsigned plane_num = 0; plane_num < registers::kImagePlaneCount; plane_num++) {
        if (allocs[pipe_num][plane_num] < min_allocs[pipe_num][plane_num]) {
          done = false;
          allocs[pipe_num][plane_num] = min_allocs[pipe_num][plane_num];
          forced_alloc[plane_num] = true;
          total_data_rate_bytes_per_frame -= data_rate_bytes_per_frame[pipe_num][plane_num];
          buffers_per_pipe -= allocs[pipe_num][plane_num];
        }
      }
    }
  }

  // Do the actual allocation, using the buffers that are assigned to each pipe.
  {
    fbl::AutoLock lock(&plane_buffers_lock_);
    const uint16_t data_buffer_block_count = DataBufferBlockCount();
    for (unsigned pipe_num = 0; pipe_num < PipeIds<registers::Platform::kKabyLake>().size();
         pipe_num++) {
      uint16_t start = pipe_buffers_[pipe_num].start;
      for (unsigned plane_num = 0; plane_num < registers::kImagePlaneCount; plane_num++) {
        auto cur = &plane_buffers_[pipe_num][plane_num];

        if (allocs[pipe_num][plane_num] == 0) {
          cur->start = data_buffer_block_count;
          cur->end = static_cast<uint16_t>(cur->start + 1);
        } else {
          cur->start = start;
          cur->end = static_cast<uint16_t>(start + allocs[pipe_num][plane_num]);
        }
        start = static_cast<uint16_t>(start + allocs[pipe_num][plane_num]);

        PipeId pipe_id = PipeIds<registers::Platform::kKabyLake>()[pipe_num];
        registers::PipeRegs pipe_regs(pipe_id);

        // These are latched on the surface address register, so we don't yet need to
        // worry about overlaps when updating planes during a pipe allocation.
        auto buf_cfg = pipe_regs.PlaneBufCfg(plane_num + 1).FromValue(0);
        buf_cfg.set_buffer_start(cur->start);
        buf_cfg.set_buffer_end(cur->end - 1);
        buf_cfg.WriteTo(mmio_space());

        // TODO(fxbug.com/111420): Follow the "Display Watermarks" guidelines.
        auto wm0 = pipe_regs.PlaneWatermark(plane_num + 1, 0).FromValue(0);
        wm0.set_enable(cur->start != data_buffer_block_count);
        wm0.set_blocks(cur->end - cur->start);
        wm0.WriteTo(mmio_space());

        // Give the buffers to both the cursor plane and plane 2, since
        // only one will actually be active.
        if (plane_num == registers::kCursorPlane) {
          auto buf_cfg = pipe_regs.PlaneBufCfg(0).FromValue(0);
          buf_cfg.set_buffer_start(cur->start);
          buf_cfg.set_buffer_end(cur->end - 1);
          buf_cfg.WriteTo(mmio_space());

          auto wm0 = pipe_regs.PlaneWatermark(0, 0).FromValue(0);
          wm0.set_enable(cur->start != data_buffer_block_count);
          wm0.set_blocks(cur->end - cur->start);
          wm0.WriteTo(mmio_space());
        }
      }
    }
  }
}

void Controller::ReallocatePlaneBuffers(cpp20::span<const display_config_t> banjo_display_configs,
                                        bool reallocate_pipes) {
  if (banjo_display_configs.empty()) {
    // Deal with reallocation later, when there are actually displays
    return;
  }

  uint16_t min_allocs[PipeIds<registers::Platform::kKabyLake>().size()]
                     [registers::kImagePlaneCount];
  if (!CalculateMinimumAllocations(banjo_display_configs, min_allocs)) {
    // The allocation should have been checked, so this shouldn't fail
    ZX_ASSERT(false);
  }

  // Calculate the data rates and store the minimum allocations
  uint64_t data_rate_bytes_per_frame[PipeIds<registers::Platform::kKabyLake>().size()]
                                    [registers::kImagePlaneCount];
  for (Pipe* pipe : *pipe_manager_) {
    PipeId pipe_id = pipe->pipe_id();
    for (unsigned plane_num = 0; plane_num < registers::kImagePlaneCount; plane_num++) {
      const layer_t* layer;
      if (!GetPlaneLayer(pipe, plane_num, banjo_display_configs, &layer)) {
        data_rate_bytes_per_frame[pipe_id][plane_num] = 0;
      } else if (layer->type == LAYER_TYPE_PRIMARY) {
        const primary_layer_t* primary = &layer->cfg.primary;

        uint32_t scaled_width = primary->image_source.width * primary->image_source.width /
                                primary->display_destination.width;
        uint32_t scaled_height = primary->image_source.height * primary->image_source.height /
                                 primary->display_destination.height;

        // TODO(https://fxbug.dev/42076788): Currently we assume only RGBA/BGRA formats
        // are supported and hardcode the bytes-per-pixel value to avoid pixel
        // format check and stride calculation (which requires holding the GTT
        // lock). This may change when we need to support non-RGBA/BGRA images.
        constexpr int bytes_per_pixel = 4;
        // Plane buffers are recalculated only on valid configurations. So all
        // images must be valid.
        const display::DriverImageId primary_image_id(primary->image_handle);
        ZX_DEBUG_ASSERT(primary_image_id != display::kInvalidDriverImageId);
        ZX_DEBUG_ASSERT(bytes_per_pixel == ImageFormatStrideBytesPerWidthPixel(
                                               GetImportedImagePixelFormat(primary_image_id)));

        data_rate_bytes_per_frame[pipe_id][plane_num] =
            uint64_t{scaled_width} * scaled_height * bytes_per_pixel;
      } else {
        // Other layers don't use pipe/planes, so GetPlaneLayer should have returned false
        ZX_ASSERT(false);
      }
    }
  }

  if (initial_alloc_) {
    initial_alloc_ = false;
    reallocate_pipes = true;
  }

  buffer_allocation_t active_allocation[PipeIds<registers::Platform::kKabyLake>().size()];
  if (reallocate_pipes) {
    // Allocate buffers to each pipe, but save the old allocation to use
    // when progressively updating the allocation.
    memcpy(active_allocation, pipe_buffers_, sizeof(active_allocation));

    size_t active_pipes = std::count_if(pipe_manager_->begin(), pipe_manager_->end(),
                                        [](const Pipe* pipe) { return pipe->in_use(); });
    uint16_t buffers_per_pipe = CalculateBuffersPerPipe(active_pipes);

    int current_active_pipe = 0;
    for (Pipe* pipe : *pipe_manager_) {
      PipeId pipe_id = pipe->pipe_id();
      if (pipe->in_use()) {
        pipe_buffers_[pipe_id].start =
            static_cast<uint16_t>(buffers_per_pipe * current_active_pipe);
        pipe_buffers_[pipe_id].end =
            static_cast<uint16_t>(pipe_buffers_[pipe_id].start + buffers_per_pipe);
        current_active_pipe++;
      } else {
        pipe_buffers_[pipe_id].start = pipe_buffers_[pipe_id].end = 0;
      }
      FDF_LOG(INFO, "Pipe %d buffers: [%d, %d)", pipe_id, pipe_buffers_[pipe_id].start,
              pipe_buffers_[pipe_id].end);
    }
  }

  // It's not necessary to flush the buffer changes since the pipe allocs didn't change
  UpdateAllocations(min_allocs, data_rate_bytes_per_frame);

  if (reallocate_pipes) {
    DoPipeBufferReallocation(active_allocation);
  }
}

void Controller::DoPipeBufferReallocation(
    buffer_allocation_t active_allocation[PipeIds<registers::Platform::kKabyLake>().size()]) {
  // Given that the order of the allocations is fixed, an allocation X_i is contained completely
  // within its old allocation if {new len of allocations preceding X_i} >= {start of old X_i} and
  // {new len of allocations preceding X_i + new len of X_i} <= {end of old X_i}. For any i,
  // if condition 1 holds, either condition 2 is true and we're done, or condition 2 doesn't
  // and condition 1 holds for i + 1. Since condition 1 holds for i == 0 and because condition
  // 2 holds for the last allocation (since the allocation is valid), it is guaranteed that
  // at least one allocation is entirely within its old allocation. The remaining buffers
  // are guaranteed to be re-allocatable recursively in the same manner. Therefore the loop will
  // make progress every iteration.
  bool done = false;
  while (!done) {
    done = true;
    for (unsigned pipe_num = 0; pipe_num < PipeIds<registers::Platform::kKabyLake>().size();
         pipe_num++) {
      auto active_alloc = active_allocation + pipe_num;
      auto goal_alloc = pipe_buffers_ + pipe_num;

      if (active_alloc->start == goal_alloc->start && active_alloc->end == goal_alloc->end) {
        continue;
      }

      // Look through all the other active pipe allocations for overlap
      bool overlap = false;
      if (goal_alloc->start != goal_alloc->end) {
        for (unsigned other_pipe = 0; other_pipe < PipeIds<registers::Platform::kKabyLake>().size();
             other_pipe++) {
          if (other_pipe == pipe_num) {
            continue;
          }

          auto other_active = active_allocation + other_pipe;
          if (other_active->start == other_active->end) {
            continue;
          }

          if ((other_active->start <= goal_alloc->start && goal_alloc->start < other_active->end) ||
              (other_active->start < goal_alloc->end && goal_alloc->end <= other_active->end)) {
            overlap = true;
            break;
          }
        }
      }

      if (!overlap) {
        // Flush the pipe allocation, wait for it to be active, and update
        // what is current active.
        registers::PipeRegs pipe_regs(PipeIds<registers::Platform::kKabyLake>()[pipe_num]);
        for (unsigned j = 0; j < registers::kImagePlaneCount; j++) {
          pipe_regs.PlaneSurface(j).ReadFrom(mmio_space()).WriteTo(mmio_space());
        }
        pipe_regs.CursorBase().ReadFrom(mmio_space()).WriteTo(mmio_space());

        // TODO(stevensd): Wait for vsync instead of sleeping
        // TODO(stevesnd): Parallelize/reduce the number of vsyncs we wait for
        zx_nanosleep(zx_deadline_after(ZX_MSEC(33)));

        *active_alloc = *goal_alloc;
      } else {
        done = false;
      }
    }
  }
}

bool Controller::CheckDisplayLimits(
    cpp20::span<const display_config_t> banjo_display_configs,
    cpp20::span<client_composition_opcode_t> client_composition_opcodes) {
  int client_composition_opcodes_offset = 0;
  for (unsigned i = 0; i < banjo_display_configs.size(); i++) {
    const display_config_t& banjo_display_config = banjo_display_configs[i];
    ZX_DEBUG_ASSERT(client_composition_opcodes.size() >=
                    client_composition_opcodes_offset + banjo_display_config.layer_count);
    cpp20::span<client_composition_opcode_t> current_display_client_composition_opcodes =
        client_composition_opcodes.subspan(client_composition_opcodes_offset,
                                           banjo_display_config.layer_count);
    client_composition_opcodes_offset += banjo_display_config.layer_count;

    const display::DisplayTiming display_timing =
        display::ToDisplayTiming(banjo_display_config.mode);
    // The intel display controller doesn't support these flags
    if (display_timing.vblank_alternates) {
      return false;
    }
    if (display_timing.pixel_repetition > 0) {
      return false;
    }

    display::DisplayId display_id = display::ToDisplayId(banjo_display_config.display_id);
    DisplayDevice* display = FindDevice(display_id);
    if (display == nullptr) {
      continue;
    }

    // Pipes don't support height of more than 4096. They support a width of up to
    // 2^14 - 1. However, planes don't support a width of more than 8192 and we need
    // to always be able to accept a single plane, fullscreen configuration.
    if (display_timing.vertical_active_lines > 4096 || display_timing.horizontal_active_px > 8192) {
      return false;
    }

    int64_t max_pipe_pixel_rate_hz;
    auto cd_freq_khz = registers::CdClockCtl::Get().ReadFrom(mmio_space()).cd_freq_decimal();

    if (cd_freq_khz == registers::CdClockCtl::FreqDecimal(307'200)) {
      max_pipe_pixel_rate_hz = 307'200'000;
    } else if (cd_freq_khz == registers::CdClockCtl::FreqDecimal(308'570)) {
      max_pipe_pixel_rate_hz = 308'570'000;
    } else if (cd_freq_khz == registers::CdClockCtl::FreqDecimal(337'500)) {
      max_pipe_pixel_rate_hz = 337'500'000;
    } else if (cd_freq_khz == registers::CdClockCtl::FreqDecimal(432'000)) {
      max_pipe_pixel_rate_hz = 432'000'000;
    } else if (cd_freq_khz == registers::CdClockCtl::FreqDecimal(450'000)) {
      max_pipe_pixel_rate_hz = 450'000'000;
    } else if (cd_freq_khz == registers::CdClockCtl::FreqDecimal(540'000)) {
      max_pipe_pixel_rate_hz = 540'000'000;
    } else if (cd_freq_khz == registers::CdClockCtl::FreqDecimal(617'140)) {
      max_pipe_pixel_rate_hz = 617'140'000;
    } else if (cd_freq_khz == registers::CdClockCtl::FreqDecimal(675'000)) {
      max_pipe_pixel_rate_hz = 675'000'000;
    } else {
      ZX_ASSERT(false);
    }

    // Either the pipe pixel rate or the link pixel rate can't support a simple
    // configuration at this display resolution.
    const int64_t pixel_clock_hz = banjo_display_config.mode.pixel_clock_hz;
    if (max_pipe_pixel_rate_hz < pixel_clock_hz || !display->CheckPixelRate(pixel_clock_hz)) {
      return false;
    }

    // Compute the maximum pipe pixel rate with the desired scaling. If the max rate
    // is too low, then make the client do any downscaling itself.
    double min_plane_ratio = 1.0;
    for (unsigned i = 0; i < banjo_display_config.layer_count; i++) {
      if (banjo_display_config.layer_list[i].type != LAYER_TYPE_PRIMARY) {
        continue;
      }
      const primary_layer_t* primary = &banjo_display_config.layer_list[i].cfg.primary;
      uint32_t src_width, src_height;
      GetPostTransformWidth(banjo_display_config.layer_list[i], &src_width, &src_height);

      double downscale = std::max(1.0, 1.0 * src_height / primary->display_destination.height) *
                         std::max(1.0, 1.0 * src_width / primary->display_destination.width);
      double plane_ratio = 1.0 / downscale;
      min_plane_ratio = std::min(plane_ratio, min_plane_ratio);
    }

    max_pipe_pixel_rate_hz =
        static_cast<int64_t>(min_plane_ratio * static_cast<double>(max_pipe_pixel_rate_hz));
    if (max_pipe_pixel_rate_hz < pixel_clock_hz) {
      for (unsigned j = 0; j < banjo_display_config.layer_count; j++) {
        if (banjo_display_config.layer_list[j].type != LAYER_TYPE_PRIMARY) {
          continue;
        }
        const primary_layer_t* primary = &banjo_display_config.layer_list[j].cfg.primary;
        uint32_t src_width, src_height;
        GetPostTransformWidth(banjo_display_config.layer_list[j], &src_width, &src_height);

        if (src_height > primary->display_destination.height ||
            src_width > primary->display_destination.width) {
          current_display_client_composition_opcodes[j] |= CLIENT_COMPOSITION_OPCODE_FRAME_SCALE;
        }
      }
    }

    // TODO(stevensd): Check maximum memory read bandwidth, watermark
  }

  return true;
}

config_check_result_t Controller::DisplayEngineCheckConfiguration(
    const display_config_t* banjo_display_configs, size_t display_config_count,
    client_composition_opcode_t* out_client_composition_opcodes_list,
    size_t client_composition_opcodes_count, size_t* out_client_composition_opcodes_actual) {
  fbl::AutoLock lock(&display_lock_);

  if (out_client_composition_opcodes_actual != nullptr) {
    *out_client_composition_opcodes_actual = 0;
  }

  cpp20::span banjo_display_configs_span(banjo_display_configs, display_config_count);
  if (banjo_display_configs_span.empty()) {
    // All displays off is supported
    return CONFIG_CHECK_RESULT_OK;
  }

  std::array<display::DisplayId, PipeIds<registers::Platform::kKabyLake>().size()>
      display_allocated_to_pipe;
  if (!CalculatePipeAllocation(banjo_display_configs_span, display_allocated_to_pipe)) {
    return CONFIG_CHECK_RESULT_TOO_MANY;
  }

  int total_layer_count = std::accumulate(
      banjo_display_configs, banjo_display_configs + display_config_count, 0,
      [](int total, const display_config_t& config) { return total += config.layer_count; });
  ZX_DEBUG_ASSERT(client_composition_opcodes_count >= static_cast<size_t>(total_layer_count));
  cpp20::span<client_composition_opcode_t> client_composition_opcodes(
      out_client_composition_opcodes_list, total_layer_count);
  std::fill(client_composition_opcodes.begin(), client_composition_opcodes.end(), 0);
  if (out_client_composition_opcodes_actual != nullptr) {
    *out_client_composition_opcodes_actual = total_layer_count;
  }

  if (!CheckDisplayLimits(banjo_display_configs_span, client_composition_opcodes)) {
    return CONFIG_CHECK_RESULT_UNSUPPORTED_MODES;
  }

  int client_composition_opcodes_offset = 0;
  for (unsigned i = 0; i < banjo_display_configs_span.size(); i++) {
    const display_config_t& banjo_display_config = banjo_display_configs_span[i];
    cpp20::span<client_composition_opcode_t> current_display_client_composition_opcodes =
        client_composition_opcodes.subspan(client_composition_opcodes_offset,
                                           banjo_display_config.layer_count);
    client_composition_opcodes_offset += banjo_display_config.layer_count;

    const display::DisplayId display_id = display::ToDisplayId(banjo_display_config.display_id);
    DisplayDevice* display = nullptr;
    for (auto& d : display_devices_) {
      if (d->id() == display_id) {
        display = d.get();
        break;
      }
    }
    if (display == nullptr) {
      FDF_LOG(INFO, "Got config with no display - assuming hotplug and skipping");
      continue;
    }

    bool merge_all = false;
    if (banjo_display_config.layer_count > 3) {
      merge_all = banjo_display_config.layer_count > 4 ||
                  banjo_display_config.layer_list[0].type != LAYER_TYPE_COLOR;
    }
    if (!merge_all && banjo_display_config.cc_flags) {
      if (banjo_display_config.cc_flags & COLOR_CONVERSION_PREOFFSET) {
        for (int i = 0; i < 3; i++) {
          merge_all |= banjo_display_config.cc_preoffsets[i] <= -1;
          merge_all |= banjo_display_config.cc_preoffsets[i] >= 1;
        }
      }
      if (banjo_display_config.cc_flags & COLOR_CONVERSION_POSTOFFSET) {
        for (int i = 0; i < 3; i++) {
          merge_all |= banjo_display_config.cc_postoffsets[i] <= -1;
          merge_all |= banjo_display_config.cc_postoffsets[i] >= 1;
        }
      }
    }

    uint32_t total_scalers_needed = 0;
    for (unsigned j = 0; j < banjo_display_config.layer_count; j++) {
      switch (banjo_display_config.layer_list[j].type) {
        case LAYER_TYPE_PRIMARY: {
          const primary_layer_t* primary = &banjo_display_config.layer_list[j].cfg.primary;
          if (primary->image_source_transformation == COORDINATE_TRANSFORMATION_ROTATE_CCW_90 ||
              primary->image_source_transformation == COORDINATE_TRANSFORMATION_ROTATE_CCW_270) {
            // Linear and x tiled images don't support 90/270 rotation
            if (primary->image_metadata.tiling_type == IMAGE_TILING_TYPE_LINEAR ||
                primary->image_metadata.tiling_type == IMAGE_TILING_TYPE_X_TILED) {
              current_display_client_composition_opcodes[j] |= CLIENT_COMPOSITION_OPCODE_TRANSFORM;
            }
          } else if (primary->image_source_transformation != COORDINATE_TRANSFORMATION_IDENTITY &&
                     primary->image_source_transformation !=
                         COORDINATE_TRANSFORMATION_ROTATE_CCW_180) {
            // Cover unsupported rotations
            current_display_client_composition_opcodes[j] |= CLIENT_COMPOSITION_OPCODE_TRANSFORM;
          }

          uint32_t src_width, src_height;
          GetPostTransformWidth(banjo_display_config.layer_list[j], &src_width, &src_height);

          // If the plane is too wide, force the client to do all composition
          // and just give us a simple configuration.
          uint32_t max_width;
          if (primary->image_metadata.tiling_type == IMAGE_TILING_TYPE_LINEAR ||
              primary->image_metadata.tiling_type == IMAGE_TILING_TYPE_X_TILED) {
            max_width = 8192;
          } else {
            max_width = 4096;
          }
          if (src_width > max_width) {
            merge_all = true;
          }

          if (primary->display_destination.width != src_width ||
              primary->display_destination.height != src_height) {
            float ratio = registers::PipeScalerControlSkylake::k7x5MaxRatio;
            uint32_t max_width = static_cast<uint32_t>(static_cast<float>(src_width) * ratio);
            uint32_t max_height = static_cast<uint32_t>(static_cast<float>(src_height) * ratio);
            uint32_t scalers_needed = 1;
            // The 7x5 scaler (i.e. 2 scaler resources) is required if the src width is
            // >2048 and the required vertical scaling is greater than 1.99.
            if (primary->image_source.width > 2048) {
              float ratio = registers::PipeScalerControlSkylake::kDynamicMaxVerticalRatio2049;
              uint32_t max_dynamic_height =
                  static_cast<uint32_t>(static_cast<float>(src_height) * ratio);
              if (max_dynamic_height < primary->display_destination.height) {
                scalers_needed = 2;
              }
            }

            // Verify that there are enough scaler resources
            // Verify that the scaler input isn't too large or too small
            // Verify that the required scaling ratio isn't too large
            bool using_c = display_allocated_to_pipe[PipeId::PIPE_C] == display->id();
            if ((total_scalers_needed + scalers_needed) >
                    (using_c ? registers::PipeScalerControlSkylake::kPipeCScalersAvailable
                             : registers::PipeScalerControlSkylake::kPipeABScalersAvailable) ||
                src_width > registers::PipeScalerControlSkylake::kMaxSrcWidthPx ||
                src_width < registers::PipeScalerControlSkylake::kMinSrcSizePx ||
                src_height < registers::PipeScalerControlSkylake::kMinSrcSizePx ||
                max_width < primary->display_destination.width ||
                max_height < primary->display_destination.height) {
              current_display_client_composition_opcodes[j] |=
                  CLIENT_COMPOSITION_OPCODE_FRAME_SCALE;
            } else {
              total_scalers_needed += scalers_needed;
            }
          }
          break;
        }
        case LAYER_TYPE_COLOR: {
          if (j != 0) {
            current_display_client_composition_opcodes[j] |= CLIENT_COMPOSITION_OPCODE_USE_PRIMARY;
          }
          const auto format = static_cast<fuchsia_images2::wire::PixelFormat>(
              banjo_display_config.layer_list[j].cfg.color.color.format);
          if (format != fuchsia_images2::wire::PixelFormat::kB8G8R8A8 &&
              format != fuchsia_images2::wire::PixelFormat::kR8G8B8A8) {
            current_display_client_composition_opcodes[j] |= CLIENT_COMPOSITION_OPCODE_USE_PRIMARY;
          }
          break;
        }
        default:
          current_display_client_composition_opcodes[j] |= CLIENT_COMPOSITION_OPCODE_USE_PRIMARY;
      }
    }

    if (merge_all) {
      current_display_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_MERGE_BASE;
      for (unsigned j = 1; j < banjo_display_config.layer_count; j++) {
        current_display_client_composition_opcodes[j] = CLIENT_COMPOSITION_OPCODE_MERGE_SRC;
      }
    }
  }

  // CalculateMinimumAllocations ignores layers after kImagePlaneCount. That's fine, since
  // that case already fails from an earlier check.
  uint16_t arr[PipeIds<registers::Platform::kKabyLake>().size()][registers::kImagePlaneCount];
  if (!CalculateMinimumAllocations(banjo_display_configs_span, arr)) {
    // Find any displays whose allocation fails and set the return code. Overwrite
    // any previous errors, since they get solved by the merge.
    for (Pipe* pipe : *pipe_manager_) {
      PipeId pipe_id = pipe->pipe_id();
      if (arr[pipe_id][0] != UINT16_MAX) {
        continue;
      }
      ZX_ASSERT(pipe->in_use());  // If the allocation failed, it should be in use
      display::DisplayId pipe_attached_display_id = pipe->attached_display_id();

      int client_composition_opcodes_offset = 0;
      for (unsigned i = 0; i < display_config_count; i++) {
        cpp20::span<client_composition_opcode_t> current_display_client_composition_opcodes =
            client_composition_opcodes.subspan(client_composition_opcodes_offset,
                                               banjo_display_configs[i].layer_count);
        client_composition_opcodes_offset += banjo_display_configs[i].layer_count;

        display::DisplayId display_id = display::ToDisplayId(banjo_display_configs[i].display_id);
        if (display_id != pipe_attached_display_id) {
          continue;
        }

        current_display_client_composition_opcodes[0] = CLIENT_COMPOSITION_OPCODE_MERGE_BASE;
        for (unsigned j = 1; j < banjo_display_configs[i].layer_count; j++) {
          current_display_client_composition_opcodes[j] = CLIENT_COMPOSITION_OPCODE_MERGE_SRC;
        }
        break;
      }
    }
  }
  return CONFIG_CHECK_RESULT_OK;
}

bool Controller::CalculatePipeAllocation(
    cpp20::span<const display_config_t> banjo_display_configs,
    cpp20::span<display::DisplayId> display_allocated_to_pipe) {
  ZX_DEBUG_ASSERT(display_allocated_to_pipe.size() ==
                  PipeIds<registers::Platform::kKabyLake>().size());
  if (banjo_display_configs.size() > display_allocated_to_pipe.size()) {
    return false;
  }
  std::fill(display_allocated_to_pipe.begin(), display_allocated_to_pipe.end(),
            display::kInvalidDisplayId);
  // Keep any allocated pipes on the same display
  for (const display_config_t& banjo_display_config : banjo_display_configs) {
    display::DisplayId display_id = display::ToDisplayId(banjo_display_config.display_id);
    DisplayDevice* display = FindDevice(display_id);
    if (display != nullptr && display->pipe() != nullptr) {
      display_allocated_to_pipe[display->pipe()->pipe_id()] = display_id;
    }
  }
  // Give unallocated pipes to displays that need them
  for (const display_config_t& banjo_display_config : banjo_display_configs) {
    display::DisplayId display_id = display::ToDisplayId(banjo_display_config.display_id);
    DisplayDevice* display = FindDevice(display_id);
    if (display != nullptr && display->pipe() == nullptr) {
      for (unsigned pipe_num = 0; pipe_num < display_allocated_to_pipe.size(); pipe_num++) {
        if (!display_allocated_to_pipe[pipe_num]) {
          display_allocated_to_pipe[pipe_num] = display_id;
          break;
        }
      }
    }
  }
  return true;
}

uint16_t Controller::DataBufferBlockCount() const {
  // Data buffer sizes are documented in the "Display Buffer Programming" >
  // "Display Buffer Size" section in the display engine PRMs.

  // Kaby Lake and Skylake display engines have a single DBUF slice with
  // 892 blocks.
  // Kaby Lake: IHD-OS-KBL-Vol 12-1.17 page 167
  // Skylake: IHD-OS-KBL-Vol 12-1.17 page 164
  static constexpr uint16_t kKabyLakeDataBufferBlockCount = 892;

  // Tiger Lake display engines have two DBUF slice with 1024 blocks each.
  // TODO(https://fxbug.dev/42063006): We should be able to use 2048 blocks, since wee
  // power up both slices.
  // Tiger Lake: IHD-OS-TGL-Vol 12-1.22-Rev2.0 page 297
  // DG1: IHD-OS-DG1-Vol 12-2.21 page 250
  static constexpr uint16_t kTigerLakeDataBufferBlockCount = 1023;

  return is_tgl(device_id_) ? kTigerLakeDataBufferBlockCount : kKabyLakeDataBufferBlockCount;
}

void Controller::DisplayEngineApplyConfiguration(const display_config_t* banjo_display_configs,
                                                 size_t display_config_count,
                                                 const config_stamp_t* banjo_config_stamp) {
  fbl::AutoLock lock(&display_lock_);
  ZX_DEBUG_ASSERT(display_devices_.size() <= kMaximumConnectedDisplayCount);
  display::DisplayId fake_vsync_display_ids[kMaximumConnectedDisplayCount];
  size_t fake_vsync_size = 0;

  cpp20::span<const display_config_t> banjo_display_configs_span(banjo_display_configs,
                                                                 display_config_count);
  ReallocatePlaneBuffers(banjo_display_configs_span,
                         /* reallocate_pipes */ pipe_manager_->PipeReallocated());

  for (std::unique_ptr<DisplayDevice>& display : display_devices_) {
    const display_config_t* banjo_display_config =
        FindBanjoConfig(display->id(), banjo_display_configs_span);

    if (banjo_display_config != nullptr) {
      const display::ConfigStamp config_stamp = display::ToConfigStamp(*banjo_config_stamp);
      display->ApplyConfiguration(banjo_display_config, config_stamp);
    } else {
      if (display->pipe()) {
        // Only reset the planes so that it will display a blank screen.
        display->pipe()->ResetPlanes();
        ResetPipePlaneBuffers(display->pipe()->pipe_id());
      }
    }

    // The hardware only gives vsyncs if at least one plane is enabled, so
    // fake one if we need to, to inform the client that we're done with the
    // images.
    if (!banjo_display_config || banjo_display_config->layer_count == 0) {
      fake_vsync_display_ids[fake_vsync_size++] = display->id();
    }
  }

  if (engine_listener_.is_valid()) {
    zx_time_t now = (fake_vsync_size > 0) ? zx_clock_get_monotonic() : 0;
    for (size_t i = 0; i < fake_vsync_size; i++) {
      const uint64_t banjo_display_id = display::ToBanjoDisplayId(fake_vsync_display_ids[i]);
      engine_listener_.OnDisplayVsync(banjo_display_id, now, banjo_config_stamp);
    }
  }
}

zx_status_t Controller::DisplayEngineSetBufferCollectionConstraints(
    const image_buffer_usage_t* usage, uint64_t banjo_driver_buffer_collection_id) {
  display::DriverBufferCollectionId driver_buffer_collection_id =
      display::ToDriverBufferCollectionId(banjo_driver_buffer_collection_id);
  const auto it = buffer_collections_.find(driver_buffer_collection_id);
  if (it == buffer_collections_.end()) {
    FDF_LOG(ERROR,
            "SetBufferCollectionConstraints: Cannot find imported buffer collection (id=%lu)",
            driver_buffer_collection_id.value());
    return ZX_ERR_NOT_FOUND;
  }
  const fidl::WireSyncClient<fuchsia_sysmem2::BufferCollection>& collection = it->second;

  fidl::Arena arena;

  // Loop over all combinations of supported image types and pixel formats, adding
  // an image format constraints for each unless the config is asking for a specific
  // format or type.
  std::vector<fuchsia_sysmem2::wire::ImageFormatConstraints> image_constraints_vec;
  for (uint32_t image_tiling_type : kImageTilingTypes) {
    // Skip if image type was specified and different from current type. This
    // makes it possible for a different participant to select preferred
    // modifiers.
    if (usage->tiling_type != IMAGE_TILING_TYPE_LINEAR && usage->tiling_type != image_tiling_type) {
      continue;
    }
    for (fuchsia_images2::wire::PixelFormat pixel_format_type : kPixelFormatTypes) {
      auto image_constraints = fuchsia_sysmem2::wire::ImageFormatConstraints::Builder(arena);

      image_constraints.pixel_format(pixel_format_type);
      switch (image_tiling_type) {
        case IMAGE_TILING_TYPE_LINEAR:
          image_constraints
              .pixel_format_modifier(fuchsia_images2::wire::PixelFormatModifier::kLinear)
              .bytes_per_row_divisor(64)
              .start_offset_divisor(64);
          break;
        case IMAGE_TILING_TYPE_X_TILED:
          image_constraints
              .pixel_format_modifier(fuchsia_images2::wire::PixelFormatModifier::kIntelI915XTiled)
              .bytes_per_row_divisor(4096)
              .start_offset_divisor(1);  // Not meaningful
          break;
        case IMAGE_TILING_TYPE_Y_LEGACY_TILED:
          image_constraints
              .pixel_format_modifier(fuchsia_images2::wire::PixelFormatModifier::kIntelI915YTiled)
              .bytes_per_row_divisor(4096)
              .start_offset_divisor(1);  // Not meaningful
          break;
        case IMAGE_TILING_TYPE_YF_TILED:
          image_constraints
              .pixel_format_modifier(fuchsia_images2::wire::PixelFormatModifier::kIntelI915YfTiled)
              .bytes_per_row_divisor(4096)
              .start_offset_divisor(1);  // Not meaningful
          break;
      }
      image_constraints.color_spaces(std::vector{fuchsia_images2::wire::ColorSpace::kSrgb});
      image_constraints_vec.push_back(image_constraints.Build());
    }
  }
  if (image_constraints_vec.empty()) {
    FDF_LOG(ERROR, "Config has unsupported tiling type %" PRIu32, usage->tiling_type);
    return ZX_ERR_INVALID_ARGS;
  }
  for (unsigned i = 0; i < std::size(kYuvPixelFormatTypes); ++i) {
    auto image_constraints = fuchsia_sysmem2::wire::ImageFormatConstraints::Builder(arena);
    image_constraints.pixel_format(kYuvPixelFormatTypes[i]);
    image_constraints.color_spaces(std::vector{fuchsia_images2::wire::ColorSpace::kRec709});
    image_constraints_vec.push_back(image_constraints.Build());
  }

  auto constraints = fuchsia_sysmem2::wire::BufferCollectionConstraints::Builder(arena)
                         .usage(fuchsia_sysmem2::wire::BufferUsage::Builder(arena)
                                    .display(fuchsia_sysmem2::kDisplayUsageLayer)
                                    .Build())
                         .buffer_memory_constraints(
                             fuchsia_sysmem2::wire::BufferMemoryConstraints::Builder(arena)
                                 .min_size_bytes(0)
                                 .max_size_bytes(0xffffffff)
                                 .physically_contiguous_required(false)
                                 .secure_required(false)
                                 .ram_domain_supported(true)
                                 .cpu_domain_supported(false)
                                 .inaccessible_domain_supported(false)
                                 .permitted_heaps(std::vector{
                                     fuchsia_sysmem2::wire::Heap::Builder(arena)
                                         .heap_type(bind_fuchsia_sysmem_heap::HEAP_TYPE_SYSTEM_RAM)
                                         .id(0)
                                         .Build()})
                                 .Build())
                         .image_format_constraints(image_constraints_vec);

  auto result = collection->SetConstraints(
      fuchsia_sysmem2::wire::BufferCollectionSetConstraintsRequest::Builder(arena)
          .constraints(constraints.Build())
          .Build());

  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to set constraints, %s", result.FormatDescription().c_str());
    return result.status();
  }

  return ZX_OK;
}

// Intel GPU core methods

zx_status_t Controller::IntelGpuCoreReadPciConfig16(uint16_t addr, uint16_t* value_out) {
  return pci_.ReadConfig16(addr, value_out);
}

zx_status_t Controller::IntelGpuCoreMapPciMmio(uint32_t pci_bar, uint8_t** addr_out,
                                               uint64_t* size_out) {
  if (pci_bar > fuchsia_hardware_pci::wire::kMaxBarCount) {
    return ZX_ERR_INVALID_ARGS;
  }
  fbl::AutoLock lock(&bar_lock_);
  if (!mapped_bars_[pci_bar]) {
    zx_status_t status =
        pci_.MapMmio(pci_bar, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mapped_bars_[pci_bar]);
    if (status != ZX_OK) {
      return status;
    }
  }

  // TODO(https://fxbug.dev/42133972): Add MMIO_PTR to cast. This cannot be done as long as
  // IntelGpuCoreMapPciMmio is a signature provided by banjo.
  *addr_out = reinterpret_cast<uint8_t*>(reinterpret_cast<uintptr_t>(mapped_bars_[pci_bar]->get()));
  *size_out = mapped_bars_[pci_bar]->get_size();
  return ZX_OK;
}

zx_status_t Controller::IntelGpuCoreUnmapPciMmio(uint32_t pci_bar) {
  if (pci_bar > fuchsia_hardware_pci::wire::kMaxBarCount) {
    return ZX_ERR_INVALID_ARGS;
  }
  // No work needs to be done with MmioBuffers in use.
  return ZX_OK;
}

zx_status_t Controller::IntelGpuCoreGetPciBti(uint32_t index, zx::bti* bti_out) {
  return pci_.GetBti(index, bti_out);
}

zx_status_t Controller::IntelGpuCoreRegisterInterruptCallback(
    const intel_gpu_core_interrupt_t* callback, uint32_t interrupt_mask) {
  ZX_DEBUG_ASSERT(callback);
  return interrupts_.SetGpuInterruptCallback(*callback, interrupt_mask);
}

zx_status_t Controller::IntelGpuCoreUnregisterInterruptCallback() {
  constexpr intel_gpu_core_interrupt_t kNoCallback = {nullptr, nullptr};
  interrupts_.SetGpuInterruptCallback(kNoCallback, 0);
  return ZX_OK;
}

uint64_t Controller::IntelGpuCoreGttGetSize() {
  fbl::AutoLock lock(&gtt_lock_);
  return gtt_.size();
}

zx_status_t Controller::IntelGpuCoreGttAlloc(uint64_t page_count, uint64_t* addr_out) {
  uint64_t length = page_count * PAGE_SIZE;
  fbl::AutoLock lock(&gtt_lock_);
  if (length > gtt_.size()) {
    return ZX_ERR_INVALID_ARGS;
  }
  std::unique_ptr<GttRegionImpl> region;
  zx_status_t status =
      gtt_.AllocRegion(static_cast<uint32_t>(page_count * PAGE_SIZE), PAGE_SIZE, &region);
  if (status != ZX_OK) {
    return status;
  }
  *addr_out = region->base();

  imported_gtt_regions_.push_back(std::move(region));
  return ZX_OK;
}

zx_status_t Controller::IntelGpuCoreGttFree(uint64_t addr) {
  fbl::AutoLock lock(&gtt_lock_);
  for (unsigned i = 0; i < imported_gtt_regions_.size(); i++) {
    if (imported_gtt_regions_[i]->base() == addr) {
      imported_gtt_regions_.erase(i)->ClearRegion();
      return ZX_OK;
    }
  }
  return ZX_ERR_INVALID_ARGS;
}

zx_status_t Controller::IntelGpuCoreGttClear(uint64_t addr) {
  fbl::AutoLock lock(&gtt_lock_);
  for (unsigned i = 0; i < imported_gtt_regions_.size(); i++) {
    if (imported_gtt_regions_[i]->base() == addr) {
      imported_gtt_regions_[i]->ClearRegion();
      return ZX_OK;
    }
  }
  return ZX_ERR_INVALID_ARGS;
}

zx_status_t Controller::IntelGpuCoreGttInsert(uint64_t addr, zx::vmo buffer, uint64_t page_offset,
                                              uint64_t page_count) {
  fbl::AutoLock lock(&gtt_lock_);
  for (unsigned i = 0; i < imported_gtt_regions_.size(); i++) {
    if (imported_gtt_regions_[i]->base() == addr) {
      return imported_gtt_regions_[i]->PopulateRegion(buffer.release(), page_offset,
                                                      page_count * PAGE_SIZE, true /* writable */);
    }
  }
  return ZX_ERR_INVALID_ARGS;
}

// Ddk methods

void Controller::Start(fdf::StartCompleter completer) {
  FDF_LOG(TRACE, "intel-display: initializing displays");

  {
    fbl::AutoLock lock(&display_lock_);
    for (Pipe* pipe : *pipe_manager_) {
      interrupts()->EnablePipeInterrupts(pipe->pipe_id(), /*enabled=*/true);
    }
  }

  InitDisplays();

  {
    fbl::AutoLock lock(&display_lock_);

    // If `RegisterDisplayEngineListener` occurs **before** driver
    // initialization (i.e. `engine_listener_` is valid), `DdkInit()` should be
    // responsible for notifying the coordinator of existing display devices.
    //
    // Otherwise, `RegisterDisplayEngineListener` should be responsible for
    // notifying the coordinator of existing display devices.
    if ((!display_devices_.is_empty()) && engine_listener_.is_valid()) {
      for (const std::unique_ptr<DisplayDevice>& display_device : display_devices_) {
        const raw_display_info_t banjo_display_info = display_device->CreateRawDisplayInfo();
        engine_listener_.OnDisplayAdded(&banjo_display_info);
      }
    }

    driver_initialized_ = true;
  }

  interrupts_.FinishInit();

  FDF_LOG(TRACE, "intel-display: display initialization done");
  completer(zx::ok());
}

void Controller::PrepareStopOnPowerOn(fdf::PrepareStopCompleter completer) {
  {
    fbl::AutoLock lock(&display_lock_);
    display_devices_.reset();
  }

  completer(zx::ok());
}

void Controller::PrepareStopOnPowerStateTransition(
    fuchsia_system_state::SystemPowerState power_state, fdf::PrepareStopCompleter completer) {
  // TODO(https://fxbug.dev/42119483): Implement the suspend hook based on suspendtxn
  if (power_state == fuchsia_system_state::SystemPowerState::kMexec) {
    zx::result<FramebufferInfo> fb_status = GetFramebufferInfo(resources_.framebuffer);
    if (fb_status.is_error()) {
      completer(zx::ok());
      return;
    }

    // The bootloader framebuffer is most likely at the start of the display
    // controller's bar 2. Try to get that buffer working again across the
    // mexec by mapping gfx stolen memory to gaddr 0.

    auto bdsm_reg = registers::BaseDsm::Get().FromValue(0);
    zx_status_t status = pci_.ReadConfig32(bdsm_reg.kAddr, bdsm_reg.reg_value_ptr());
    if (status != ZX_OK) {
      FDF_LOG(TRACE, "Failed to read dsm base");
      completer(zx::ok());
      return;
    }

    // The Intel docs say that the first page should be reserved for the gfx
    // hardware, but a lot of BIOSes seem to ignore that.
    uintptr_t fb = bdsm_reg.base_phys_addr() << bdsm_reg.base_phys_addr_shift;
    const auto& fb_info = fb_status.value();
    {
      fbl::AutoLock lock(&gtt_lock_);
      gtt_.SetupForMexec(fb, fb_info.size);
    }

    // It may be tempting to try to map the framebuffer and clear it here.
    // However, on Tiger Lake, mapping the framebuffer BAR after setting up
    // the display engine will cause the device to crash and reboot.
    // See https://fxbug.dev/42072946.

    {
      fbl::AutoLock lock(&display_lock_);
      for (auto& display : display_devices_) {
        if (display->pipe() == nullptr) {
          continue;
        }
        // TODO(https://fxbug.dev/42106271): Reset/scale the display to ensure the buffer displays
        // properly
        registers::PipeRegs pipe_regs(display->pipe()->pipe_id());

        auto plane_stride = pipe_regs.PlaneSurfaceStride(0).ReadFrom(mmio_space());
        plane_stride.set_stride(
            width_in_tiles(IMAGE_TILING_TYPE_LINEAR, fb_info.width, fb_info.bytes_per_pixel));
        plane_stride.WriteTo(mmio_space());

        auto plane_surface = pipe_regs.PlaneSurface(0).ReadFrom(mmio_space());
        plane_surface.set_surface_base_addr(0);
        plane_surface.WriteTo(mmio_space());
      }
    }
  }
  completer(zx::ok());
}

zx_koid_t GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

zx_status_t Controller::Init() {
  FDF_LOG(TRACE, "Binding to display controller");

  auto pid = GetKoid(zx_process_self());
  std::string debug_name = fxl::StringPrintf("intel-display[%lu]", pid);
  fidl::Arena arena;
  auto set_debug_status = sysmem_->SetDebugClientInfo(
      fuchsia_sysmem2::wire::AllocatorSetDebugClientInfoRequest::Builder(arena)
          .name(fidl::StringView::FromExternal(debug_name))
          .id(pid)
          .Build());
  if (!set_debug_status.ok()) {
    FDF_LOG(ERROR, "Cannot set sysmem allocator debug info: %s", set_debug_status.status_string());
    return set_debug_status.status();
  }

  ZX_DEBUG_ASSERT(pci_.is_valid());
  pci_.ReadConfig16(fuchsia_hardware_pci::Config::kDeviceId, &device_id_);
  FDF_LOG(TRACE, "Device id %x", device_id_);

  const zx::unowned_resource& driver_mmio_resource = resources_.mmio;
  if (!driver_mmio_resource->is_valid()) {
    FDF_LOG(WARNING, "Failed to get driver MMIO resource. VBT initialization skipped.");
  } else {
    zx_status_t status = igd_opregion_.Init(driver_mmio_resource->borrow(), pci_);
    if (status != ZX_OK && status != ZX_ERR_NOT_SUPPORTED) {
      FDF_LOG(ERROR, "VBT initializaton failed: %s", zx_status_get_string(status));
      return status;
    }
  }

  FDF_LOG(TRACE, "Mapping registers");
  // map register window
  uint8_t* regs;
  uint64_t size;
  zx_status_t status = IntelGpuCoreMapPciMmio(0u, &regs, &size);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to map bar 0: %d", status);
    return status;
  }

  {
    fbl::AutoLock lock(&bar_lock_);
    fbl::AllocChecker ac;
    mmio_space_.emplace(mapped_bars_[0]->View(0));
  }

  FDF_LOG(TRACE, "Reading fuses and straps");
  FuseConfig fuse_config = FuseConfig::ReadFrom(*mmio_space(), device_id_);
  fuse_config.Log();

  FDF_LOG(TRACE, "Initializing DDIs");
  ddis_ = GetDdiIds(device_id_);

  FDF_LOG(TRACE, "Initializing Power");
  power_ = Power::New(mmio_space(), device_id_);

  FDF_LOG(TRACE, "Reading PCH display engine config");
  pch_engine_.emplace(mmio_space(), device_id_);
  pch_engine_->Log();

  for (unsigned i = 0; i < ddis_.size(); i++) {
    gmbus_i2cs_.push_back(GMBusI2c(ddis_[i], GetPlatform(device_id_), mmio_space()));

    dp_auxs_.push_back(DpAux(mmio_space(), ddis_[i], device_id_));
    FDF_LOG(TRACE, "DDI %d AUX channel initial configuration:", ddis_[i]);
    dp_auxs_[dp_auxs_.size() - 1].aux_channel().Log();
  }

  if (!is_tgl(device_id_)) {
    ddi_e_disabled_ = registers::DdiRegs(DdiId::DDI_A)
                          .BufferControl()
                          .ReadFrom(mmio_space())
                          .ddi_e_disabled_kaby_lake();
  }

  FDF_LOG(TRACE, "Initializing interrupts");
  status = interrupts_.Init(fit::bind_member<&Controller::HandlePipeVsync>(this),
                            fit::bind_member<&Controller::HandleHotplug>(this), pci_, mmio_space(),
                            device_id_);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to initialize interrupts");
    return status;
  }

  FDF_LOG(TRACE, "Mapping gtt");
  {
    // The bootloader framebuffer is located at the start of the BAR that gets mapped by GTT.
    // Prevent clients from allocating memory in this region by telling |gtt_| to exclude it from
    // the region allocator.
    uint32_t offset = 0u;
    auto fb = GetFramebufferInfo(resources_.framebuffer);
    if (fb.is_error()) {
      FDF_LOG(INFO, "Failed to obtain framebuffer size (%s)", fb.status_string());
      // It is possible for zx_framebuffer_get_info to fail in a headless system as the bootloader
      // framebuffer information will be left uninitialized. Tolerate this failure by assuming
      // that the stolen memory contents won't be shown on any screen and map the global GTT at
      // offset 0.
      offset = 0u;
    } else {
      offset = fb.value().size;
    }

    fbl::AutoLock lock(&gtt_lock_);
    status = gtt_.Init(pci_, mmio_space()->View(GTT_BASE_OFFSET), offset);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to init gtt (%s)", zx_status_get_string(status));
      return status;
    }
  }

  {
    fbl::AutoLock lock(&display_lock_);
    if (is_tgl(device_id())) {
      pipe_manager_ = std::make_unique<PipeManagerTigerLake>(this);
    } else {
      pipe_manager_ = std::make_unique<PipeManagerSkylake>(this);
    }
  }

  if (is_tgl(device_id())) {
    ddi_manager_ = std::make_unique<DdiManagerTigerLake>(this);
  } else {
    ddi_manager_ = std::make_unique<DdiManagerSkylake>();
  }

  if (is_tgl(device_id())) {
    dpll_manager_ = std::make_unique<DpllManagerTigerLake>(mmio_space());
  } else {
    dpll_manager_ = std::make_unique<DpllManagerSkylake>(mmio_space());
  }

  root_node_ = inspector_.GetRoot().CreateChild("intel-display");
  FDF_LOG(TRACE, "bind done");
  return ZX_OK;
}

zx::result<ddk::AnyProtocol> Controller::GetProtocol(uint32_t proto_id) {
  switch (proto_id) {
    case ZX_PROTOCOL_INTEL_GPU_CORE:
      return zx::ok(ddk::AnyProtocol{
          .ops = &intel_gpu_core_protocol_ops_,
          .ctx = this,
      });
    case ZX_PROTOCOL_DISPLAY_ENGINE:
      return zx::ok(ddk::AnyProtocol{
          .ops = &display_engine_protocol_ops_,
          .ctx = this,
      });
  }
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

Controller::Controller(fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem,
                       fidl::ClientEnd<fuchsia_hardware_pci::Device> pci,
                       ControllerResources resources, inspect::Inspector inspector)
    : resources_(std::move(resources)),
      sysmem_(std::move(sysmem)),
      pci_(std::move(pci)),
      inspector_(std::move(inspector)) {}

Controller::Controller(inspect::Inspector inspector) : inspector_(std::move(inspector)) {}

Controller::~Controller() {
  interrupts_.Destroy();
  if (mmio_space() && pipe_manager_.get()) {
    for (Pipe* pipe : *pipe_manager_) {
      fbl::AutoLock lock(&display_lock_);
      interrupts()->EnablePipeInterrupts(pipe->pipe_id(), /*enable=*/true);
    }
  }
}

// static
zx::result<std::unique_ptr<Controller>> Controller::Create(
    fidl::ClientEnd<fuchsia_sysmem2::Allocator> sysmem,
    fidl::ClientEnd<fuchsia_hardware_pci::Device> pci, ControllerResources resources,
    inspect::Inspector inspector) {
  fbl::AllocChecker alloc_checker;
  auto controller =
      fbl::make_unique_checked<Controller>(&alloc_checker, std::move(sysmem), std::move(pci),
                                           std::move(resources), std::move(inspector));
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for Controller");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx_status_t status = controller->Init();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to initialize Controller: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(std::move(controller));
}

}  // namespace intel_display
