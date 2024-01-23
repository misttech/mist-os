// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-gpu.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpu.amlogic/cpp/wire.h>
#include <fidl/fuchsia.hardware.gpu.mali/cpp/wire.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/trace/event.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <zircon/errors.h>
#include <zircon/process.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/smc.h>

#include <bind/fuchsia/arm/platform/cpp/bind.h>
#include <bind/fuchsia/hardware/gpu/mali/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <soc/aml-s905d2/s905d2-hiu.h>

#include "s905d2-gpu.h"
#include "s912-gpu.h"
#include "src/devices/tee/drivers/optee/tee-smc.h"
#include "t931-gpu.h"

namespace aml_gpu {

AmlGpu::AmlGpu(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase("aml-gpu", std::move(start_args), std::move(driver_dispatcher)) {}

AmlGpu::~AmlGpu() {}

void AmlGpu::Stop() {
  if (loop_dispatcher_.get()) {
    loop_dispatcher_.ShutdownAsync();
    // At this point the Mali device has been released and won't call into this driver, so the loop
    // should shutdown quickly.
    loop_shutdown_completion_.Wait();
  }
}

void AmlGpu::SetClkFreqSource(int32_t clk_source) {
  if (current_clk_source_ == clk_source) {
    return;
  }

  FDF_LOG(INFO, "Setting clock source to %d: %d\n", clk_source,
          gpu_block_->gpu_clk_freq[clk_source]);
  uint32_t current_clk_cntl = hiu_buffer_->Read32(4 * gpu_block_->hhi_clock_cntl_offset);
  uint32_t enabled_mux = current_clk_cntl & (1 << kFinalMuxBitShift);
  uint32_t new_mux = enabled_mux == 0;
  uint32_t mux_shift = new_mux ? 16 : 0;

  // clear existing values
  current_clk_cntl &= ~(kClockMuxMask << mux_shift);
  // set the divisor, enable & source for the unused mux
  current_clk_cntl |= CalculateClockMux(true, gpu_block_->gpu_clk_freq[clk_source], 1) << mux_shift;

  // Write the new values to the unused mux
  hiu_buffer_->Write32(current_clk_cntl, 4 * gpu_block_->hhi_clock_cntl_offset);
  zx_nanosleep(zx_deadline_after(ZX_USEC(10)));

  // Toggle current mux selection
  current_clk_cntl ^= (1 << kFinalMuxBitShift);

  // Select the unused input mux
  hiu_buffer_->Write32(current_clk_cntl, 4 * gpu_block_->hhi_clock_cntl_offset);

  current_clk_source_ = clk_source;
  UpdateClockProperties();
}

void AmlGpu::SetInitialClkFreqSource(int32_t clk_source) {
  uint32_t current_clk_cntl = hiu_buffer_->Read32(4 * gpu_block_->hhi_clock_cntl_offset);
  uint32_t enabled_mux = (current_clk_cntl & (1 << kFinalMuxBitShift)) != 0;
  uint32_t mux_shift = enabled_mux ? 16 : 0;

  if (current_clk_cntl & (1 << (mux_shift + kClkEnabledBitShift))) {
    SetClkFreqSource(clk_source);
  } else {
    FDF_LOG(INFO, "Setting initial clock source to %d: %d\n", clk_source,
            gpu_block_->gpu_clk_freq[clk_source]);
    // Switching the final dynamic mux from a disabled source to an enabled
    // source doesn't work. If the current clock source is disabled, then
    // enable it instead of switching.
    current_clk_cntl &= ~(kClockMuxMask << mux_shift);
    current_clk_cntl |= CalculateClockMux(true, gpu_block_->gpu_clk_freq[clk_source], 1)
                        << mux_shift;

    // Write the new values to the existing mux.
    hiu_buffer_->Write32(current_clk_cntl, 4 * gpu_block_->hhi_clock_cntl_offset);
    zx_nanosleep(zx_deadline_after(ZX_USEC(10)));
    current_clk_source_ = clk_source;
    UpdateClockProperties();
  }
}

void AmlGpu::UpdateClockProperties() {
  current_clk_source_property_.Set(current_clk_source_);
  uint32_t clk_mux_source = gpu_block_->gpu_clk_freq[current_clk_source_];
  current_clk_mux_source_property_.Set(clk_mux_source);
  ZX_DEBUG_ASSERT(clk_mux_source < kClockInputs);
  uint32_t current_clk_freq_hz = gpu_block_->input_freq_map[clk_mux_source];
  current_clk_freq_hz_property_.Set(current_clk_freq_hz);
  TRACE_INSTANT("magma", "AmlGpu::UpdateClockProperties", TRACE_SCOPE_PROCESS, "current_clk_source",
                current_clk_source_, "clk_mux_source", clk_mux_source, "current_clk_freq_hz",
                current_clk_freq_hz);
}

zx_status_t AmlGpu::Gp0Init() {
  // hiu_dev_ is now initialized in |s905d2_hiu_init|.
  gp0_pll_dev_ = std::make_unique<aml_pll_dev_t>();

  // HIU Init.
  zx_status_t status = s905d2_hiu_init_etc(&*hiu_dev_, hiu_buffer_->View(0));
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "aml_gp0_init: hiu_init failed: %d", status);
    return status;
  }

  status = s905d2_pll_init(&*hiu_dev_, gp0_pll_dev_.get(), GP0_PLL);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "aml_gp0_init: pll_init failed: %d", status);
    return status;
  }

  status = s905d2_pll_set_rate(gp0_pll_dev_.get(), 846000000);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "aml_gp0_init: pll_set_rate failed: %d", status);
    return status;
  }
  status = s905d2_pll_ena(gp0_pll_dev_.get());
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "aml_gp0_init: pll_ena failed: %d", status);
    return status;
  }
  gp0_init_succeeded_ = true;
  root_.RecordBool("gp0_init_succeeded", true);
  return ZX_OK;
}

void AmlGpu::InitClock() {
  {
    auto result = reset_register_->WriteRegister32(gpu_block_->reset0_mask_offset,
                                                   aml_registers::MALI_RESET0_MASK, 0);
    if ((result.status() != ZX_OK) || result->is_error()) {
      FDF_LOG(ERROR, "Reset0 Mask Clear failed\n");
    }
  }

  {
    auto result = reset_register_->WriteRegister32(gpu_block_->reset0_level_offset,
                                                   aml_registers::MALI_RESET0_MASK, 0);
    if ((result.status() != ZX_OK) || result->is_error()) {
      FDF_LOG(ERROR, "Reset0 Level Clear failed\n");
    }
  }

  {
    auto result = reset_register_->WriteRegister32(gpu_block_->reset2_mask_offset,
                                                   aml_registers::MALI_RESET2_MASK, 0);
    if ((result.status() != ZX_OK) || result->is_error()) {
      FDF_LOG(ERROR, "Reset2 Mask Clear failed\n");
    }
  }

  {
    auto result = reset_register_->WriteRegister32(gpu_block_->reset2_level_offset,
                                                   aml_registers::MALI_RESET2_MASK, 0);
    if ((result.status() != ZX_OK) || result->is_error()) {
      FDF_LOG(ERROR, "Reset2 Level Clear failed\n");
    }
  }

  uint32_t initial_clock_index = gpu_block_->initial_clock_index;
  if (gpu_block_->enable_gp0 && !gp0_init_succeeded_) {
    initial_clock_index = gpu_block_->non_gp0_index;
  }

  SetInitialClkFreqSource(static_cast<int32_t>(initial_clock_index));

  {
    auto result = reset_register_->WriteRegister32(gpu_block_->reset0_level_offset,
                                                   aml_registers::MALI_RESET0_MASK,
                                                   aml_registers::MALI_RESET0_MASK);
    if ((result.status() != ZX_OK) || result->is_error()) {
      FDF_LOG(ERROR, "Reset2 Level Set failed\n");
    }
  }

  {
    auto result = reset_register_->WriteRegister32(gpu_block_->reset2_level_offset,
                                                   aml_registers::MALI_RESET2_MASK,
                                                   aml_registers::MALI_RESET2_MASK);
    if ((result.status() != ZX_OK) || result->is_error()) {
      FDF_LOG(ERROR, "Reset2 Level Set failed\n");
    }
  }

  gpu_buffer_->Write32(0x2968A819, 4 * kPwrKey);
  gpu_buffer_->Write32(0xfff | (0x20 << 16), 4 * kPwrOverride1);
}

void AmlGpu::GetProperties(fdf::Arena& arena, GetPropertiesCompleter::Sync& completer) {
  completer.buffer(arena).Reply(properties_);
}

// Match the definitions in the Amlogic OPTEE implementation.
#define DMC_DEV_ID_GPU 1

#define DMC_DEV_TYPE_NON_SECURE 0
#define DMC_DEV_TYPE_SECURE 1
#define DMC_DEV_TYPE_INACCESSIBLE 2

zx_status_t AmlGpu::SetProtected(uint32_t protection_mode) {
  if (!secure_monitor_)
    return ZX_ERR_NOT_SUPPORTED;

  // Call into the TEE to mark a particular hardware unit as able to access
  // protected memory or not.
  zx_smc_parameters_t params = {};
  zx_smc_result_t result = {};
  constexpr uint32_t kFuncIdConfigDeviceSecure = 14;
  params.func_id = tee_smc::CreateFunctionId(tee_smc::kFastCall, tee_smc::kSmc32CallConv,
                                             tee_smc::kTrustedOsService, kFuncIdConfigDeviceSecure);
  params.arg1 = DMC_DEV_ID_GPU;
  params.arg2 = protection_mode;
  zx_status_t status = zx_smc_call(secure_monitor_.get(), &params, &result);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to set unit %ld protected status %ld code: %d", params.arg1, params.arg2,
            status);
    return status;
  }
  if (result.arg0 != 0) {
    FDF_LOG(ERROR, "Failed to set unit %ld protected status %ld: %lx", params.arg1, params.arg2,
            result.arg0);
    return ZX_ERR_INTERNAL;
  }
  current_protected_mode_property_.Set(protection_mode);
  return ZX_OK;
}

void AmlGpu::EnterProtectedMode(fdf::Arena& arena, EnterProtectedModeCompleter::Sync& completer) {
  if (!secure_monitor_) {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }

  zx_status_t status = SetProtected(DMC_DEV_TYPE_SECURE);
  if (status == ZX_OK) {
    completer.buffer(arena).ReplySuccess();
  } else {
    completer.buffer(arena).ReplyError(status);
  }
}

void AmlGpu::StartExitProtectedMode(fdf::Arena& arena,
                                    StartExitProtectedModeCompleter::Sync& completer) {
  if (!secure_monitor_) {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  // Switch device to inaccessible mode. This will prevent writes to all memory
  // and start resetting the GPU.
  zx_status_t status = SetProtected(DMC_DEV_TYPE_INACCESSIBLE);
  if (status == ZX_OK) {
    completer.buffer(arena).ReplySuccess();
  } else {
    completer.buffer(arena).ReplyError(status);
  }
}

void AmlGpu::FinishExitProtectedMode(fdf::Arena& arena,
                                     FinishExitProtectedModeCompleter::Sync& completer) {
  if (!secure_monitor_) {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  // Switch to non-secure mode. This will check that the device has been reset
  // and will re-enable access to non-protected memory.
  zx_status_t status = SetProtected(DMC_DEV_TYPE_NON_SECURE);
  if (status == ZX_OK) {
    completer.buffer(arena).ReplySuccess();
  } else {
    completer.buffer(arena).ReplyError(status);
  }
}

zx_status_t AmlGpu::ProcessMetadata(
    std::vector<uint8_t> raw_metadata,
    fidl::WireTableBuilder<fuchsia_hardware_gpu_mali::wire::MaliProperties>& builder) {
  fit::result decoded = fidl::InplaceUnpersist<fuchsia_hardware_gpu_amlogic::wire::Metadata>(
      cpp20::span(raw_metadata));
  if (!decoded.is_ok()) {
    FDF_LOG(ERROR, "Unable to parse metadata %s",
            decoded.error_value().FormatDescription().c_str());
    return ZX_ERR_INTERNAL;
  }
  const auto& metadata = *decoded.value();
  builder.supports_protected_mode(metadata.has_supports_protected_mode() &&
                                  metadata.supports_protected_mode());
  return ZX_OK;
}

zx::result<> AmlGpu::Start() {
  component_inspector_ = std::make_unique<inspect::ComponentInspector>(
      dispatcher(), inspect::PublishOptions{
                        .inspector = inspector_,
                        .client_end = incoming()->Connect<fuchsia_inspect::InspectSink>().value()});

  auto loop_dispatcher = fdf::UnsynchronizedDispatcher::Create(
      fdf::UnsynchronizedDispatcher::Options{}, "aml-gpu-thread",
      [this](fdf_dispatcher_t* dispatcher) { loop_shutdown_completion_.Signal(); },
      "fuchsia.graphics.drivers.aml-gpu");

  if (!loop_dispatcher.is_ok()) {
    FDF_LOG(ERROR, "Creating dispatcher failed, status=%s\n", loop_dispatcher.status_string());
    return loop_dispatcher.take_error();
  }
  loop_dispatcher_ = *std::move(loop_dispatcher);
  root_ = inspector_.GetRoot().CreateChild("aml-gpu");
  current_clk_source_property_ = root_.CreateUint("current_clk_source", current_clk_source_);
  current_clk_mux_source_property_ = root_.CreateUint("current_clk_mux_source", 0);
  current_clk_freq_hz_property_ = root_.CreateUint("current_clk_freq_hz", 0);
  // GPU is in unknown mode on Bind.
  current_protected_mode_property_ = root_.CreateInt("current_protected_mode", -1);
  auto builder = fuchsia_hardware_gpu_mali::wire::MaliProperties::Builder(arena_);
  {
    zx::result result = incoming()->Connect<fuchsia_driver_compat::Service::Device>();
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open compat service: %s", result.status_string());
      return result.take_error();
    }
    auto compat = fidl::WireSyncClient(std::move(result.value()));
    if (!compat.is_valid()) {
      FDF_LOG(ERROR, "%s: failed to get compat", __func__);
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    auto metadata = compat->GetMetadata();
    if (!metadata.ok()) {
      FDF_LOG(ERROR, "%s: failed to GetMetadata %s", __func__, metadata.FormatDescription().data());
      return zx::error(metadata.status());
    }

    if (!metadata->is_ok()) {
      FDF_LOG(ERROR, "Metadata error: %d", metadata->error_value());
      return zx::error(metadata->error_value());
    }
    // Metadata may or may not exist; if not, default values are used.
    for (auto& entry : metadata->value()->metadata) {
      if (entry.type == fuchsia_hardware_gpu_amlogic::wire::kMaliMetadata) {
        uint64_t size;
        zx_status_t status = entry.data.get_prop_content_size(&size);
        if (status != ZX_OK) {
          FDF_LOG(ERROR, "Failed to get metadata size, %s", zx_status_get_string(status));
          return zx::error(status);
        }
        std::vector<uint8_t> raw_metadata(size);
        status = entry.data.read(raw_metadata.data(), 0, size);
        if (status != ZX_OK) {
          FDF_LOG(ERROR, "Failed to read metadata, %s", zx_status_get_string(status));
          return zx::error(status);
        }
        status = ProcessMetadata(std::move(raw_metadata), builder);
        if (status != ZX_OK) {
          FDF_LOG(ERROR, "Error processing metadata %d", status);
          return zx::error(status);
        }
        break;
      }
    }
  }

  auto platform_device =
      incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>("pdev");
  pdev_ = ddk::PDevFidl(std::move(platform_device.value()));
  if (!pdev_.is_valid()) {
    FDF_LOG(ERROR, "could not get platform device protocol");
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  zx_status_t status = pdev_.MapMmio(MMIO_GPU, &gpu_buffer_);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "MapMmio failed");
    return zx::error(status);
  }

  status = pdev_.MapMmio(MMIO_HIU, &hiu_buffer_);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "MapMmio failed");
    return zx::error(status);
  }

  pdev_device_info_t info;
  status = pdev_.GetDeviceInfo(&info);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "GetDeviceInfo failed");
    return zx::error(status);
  }

  switch (info.pid) {
    case PDEV_PID_AMLOGIC_S912:
      gpu_block_ = &s912_gpu_blocks;
      break;
    case PDEV_PID_AMLOGIC_S905D2:
    case PDEV_PID_AMLOGIC_S905D3:
      gpu_block_ = &s905d2_gpu_blocks;
      break;
    // A311D and T931 have the same GPU registers.
    case PDEV_PID_AMLOGIC_T931:
    case PDEV_PID_AMLOGIC_A311D:
      gpu_block_ = &t931_gpu_blocks;
      break;
    default:
      FDF_LOG(ERROR, "unsupported SOC PID %u\n", info.pid);
      return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto reset_register_client =
      incoming()->Connect<fuchsia_hardware_registers::Service::Device>("register-reset");
  if (reset_register_client.is_error() || !reset_register_client.value().is_valid()) {
    FDF_LOG(ERROR, "could not get register-reset fragment");
    return zx::error(ZX_ERR_NO_RESOURCES);
  }

  reset_register_ = fidl::WireSyncClient(std::move(reset_register_client.value()));

  if (info.pid == PDEV_PID_AMLOGIC_S905D3 && builder.supports_protected_mode()) {
    zx_status_t status;
    // S905D3 needs to use an SMC into the TEE to do protected mode switching.
    static constexpr uint32_t kTrustedOsSmcIndex = 0;
    status = pdev_.GetSmc(kTrustedOsSmcIndex, &secure_monitor_);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Unable to retrieve secure monitor SMC: %d", status);
      return zx::error(status);
    }
    builder.use_protected_mode_callbacks(true);
  }

  if (gpu_block_->enable_gp0) {
    zx_status_t status = Gp0Init();
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "aml_gp0_init failed: %d. Falling back to lower clock.\n", status);
      return zx::error(status);
    }
  }

  properties_ = builder.Build();

  InitClock();

  auto protocol = [this](fdf::ServerEnd<fuchsia_hardware_gpu_mali::ArmMali> server_end) mutable {
    fdf::BindServer(loop_dispatcher_.get(), std::move(server_end), this);
  };

  fuchsia_hardware_gpu_mali::Service::InstanceHandler handler({.arm_mali = std::move(protocol)});
  {
    auto status = outgoing()->AddService<fuchsia_hardware_gpu_mali::Service>(std::move(handler));
    if (status.is_error()) {
      FDF_LOG(ERROR, "%s(): Failed to add service to outgoing directory: %s\n", __func__,
              status.status_string());
      return status.take_error();
    }
  }

  fidl::Arena arena;
  node_ = fidl::WireSyncClient<fuchsia_driver_framework::Node>(std::move(node()));
  auto properties = std::vector<fuchsia_driver_framework::NodeProperty>{
      fdf::MakeProperty(bind_fuchsia_hardware_gpu_mali::SERVICE,
                        bind_fuchsia_hardware_gpu_mali::SERVICE_DRIVERTRANSPORT)};

  auto offers = std::vector{fdf::MakeOffer<fuchsia_hardware_gpu_mali::Service>("default")};

  auto args = fuchsia_driver_framework::NodeAddArgs{
      {.name = "aml-gpu", .offers = std::move(offers), .properties = std::move(properties)}};

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT(controller_endpoints.is_ok());

  auto result = node_->AddChild(fidl::ToWire(arena, std::move(args)),
                                std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
    return zx::error(result.status());
  }

  return zx::ok();
}

}  // namespace aml_gpu

// clang-format off
FUCHSIA_DRIVER_EXPORT(aml_gpu::AmlGpu);
