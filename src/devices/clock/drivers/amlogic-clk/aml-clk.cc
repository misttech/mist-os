// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-clk.h"

#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <string.h>

#include <bind/fuchsia/clock/cpp/bind.h>
#include <fbl/auto_lock.h>
#include <soc/aml-meson/aml-clk-common.h>

#include "aml-a1-blocks.h"
#include "aml-a5-blocks.h"
#include "aml-axg-blocks.h"
#include "aml-g12a-blocks.h"
#include "aml-g12b-blocks.h"
#include "aml-gxl-blocks.h"
#include "aml-sm1-blocks.h"

namespace amlogic_clock {

#define MSR_WAIT_BUSY_RETRIES 5
#define MSR_WAIT_BUSY_TIMEOUT_US 10000

zx_status_t AmlClock::PopulateRegisterBlocks(uint32_t device_id, fdf::PDev& pdev) {
  // Populate the correct register blocks.
  switch (device_id) {
    case PDEV_DID_AMLOGIC_AXG_CLK: {
      // Gauss
      gates_ = axg_clk_gates;
      gate_count_ = std::size(axg_clk_gates);
      meson_gate_enable_count_.resize(gate_count_);
      break;
    }
    case PDEV_DID_AMLOGIC_GXL_CLK: {
      gates_ = gxl_clk_gates;
      gate_count_ = std::size(gxl_clk_gates);
      meson_gate_enable_count_.resize(gate_count_);
      break;
    }
    case PDEV_DID_AMLOGIC_G12A_CLK: {
      // Astro
      clk_msr_offsets_ = g12a_clk_msr;

      clk_table_ = static_cast<const char* const*>(g12a_clk_table);
      clk_table_count_ = std::size(g12a_clk_table);

      gates_ = g12a_clk_gates;
      gate_count_ = std::size(g12a_clk_gates);
      meson_gate_enable_count_.resize(gate_count_);

      InitHiu();

      constexpr size_t cpu_clk_count = std::size(g12a_cpu_clks);
      cpu_clks_.reserve(cpu_clk_count);
      for (const auto& g12a_cpu_clk : g12a_cpu_clks) {
        cpu_clks_.emplace_back(&hiu_mmio_.value(), g12a_cpu_clk.reg, &pllclk_[g12a_cpu_clk.pll],
                               g12a_cpu_clk.initial_hz);
      }

      break;
    }
    case PDEV_DID_AMLOGIC_G12B_CLK: {
      // Sherlock
      clk_msr_offsets_ = g12b_clk_msr;

      clk_table_ = static_cast<const char* const*>(g12b_clk_table);
      clk_table_count_ = std::size(g12b_clk_table);

      gates_ = g12b_clk_gates;
      gate_count_ = std::size(g12b_clk_gates);
      meson_gate_enable_count_.resize(gate_count_);

      InitHiu();

      constexpr size_t cpu_clk_count = std::size(g12b_cpu_clks);
      cpu_clks_.reserve(cpu_clk_count);
      for (const auto& g12b_cpu_clk : g12b_cpu_clks) {
        cpu_clks_.emplace_back(&hiu_mmio_.value(), g12b_cpu_clk.reg, &pllclk_[g12b_cpu_clk.pll],
                               g12b_cpu_clk.initial_hz);
      }

      break;
    }
    case PDEV_DID_AMLOGIC_SM1_CLK: {
      // Nelson
      clk_msr_offsets_ = sm1_clk_msr;

      clk_table_ = static_cast<const char* const*>(sm1_clk_table);
      clk_table_count_ = std::size(sm1_clk_table);

      gates_ = sm1_clk_gates;
      gate_count_ = std::size(sm1_clk_gates);
      meson_gate_enable_count_.resize(gate_count_);

      muxes_ = sm1_muxes;
      mux_count_ = std::size(sm1_muxes);

      InitHiu();

      break;
    }
    case PDEV_DID_AMLOGIC_A5_CLK: {
      // AV400
      uint32_t chip_id = PDEV_PID_AMLOGIC_A5;

      zx::result smc_resource = pdev.GetSmc(0);
      if (smc_resource.is_error()) {
        FDF_LOG(ERROR, "Failed to get SMC: %s", smc_resource.status_string());
        return smc_resource.status_value();
      }

      clk_msr_offsets_ = a5_clk_msr;

      clk_table_ = static_cast<const char* const*>(a5_clk_table);
      clk_table_count_ = std::size(a5_clk_table);

      gates_ = a5_clk_gates;
      gate_count_ = std::size(a5_clk_gates);
      meson_gate_enable_count_.resize(gate_count_);

      muxes_ = a5_muxes;
      mux_count_ = std::size(a5_muxes);

      pll_count_ = a5::PLL_COUNT;
      InitHiuA5();

      constexpr size_t cpu_clk_count = std::size(a5_cpu_clks);
      cpu_clks_.reserve(cpu_clk_count);
      // For A5, there is only 1 CPU clock
      cpu_clks_.emplace_back(&hiu_mmio_.value(), a5_cpu_clks[0].reg, &pllclk_[a5_cpu_clks[0].pll],
                             a5_cpu_clks[0].initial_hz, chip_id, std::move(smc_resource.value()));

      break;
    }
    case PDEV_DID_AMLOGIC_A1_CLK: {
      // clover
      uint32_t chip_id = PDEV_PID_AMLOGIC_A1;
      clk_msr_offsets_ = a1_clk_msr;

      clk_table_ = static_cast<const char* const*>(a1_clk_table);
      clk_table_count_ = std::size(a1_clk_table);

      gates_ = a1_clk_gates;
      gate_count_ = std::size(a1_clk_gates);
      meson_gate_enable_count_.resize(gate_count_);

      muxes_ = a1_muxes;
      mux_count_ = std::size(a1_muxes);

      pll_count_ = a1::PLL_COUNT;
      InitHiuA1();

      constexpr size_t cpu_clk_count = std::size(a1_cpu_clks);
      cpu_clks_.reserve(cpu_clk_count);
      // For A1, there is only 1 CPU clock
      cpu_clks_.emplace_back(&cpuctrl_mmio_.value(), a1_cpu_clks[0].reg,
                             &pllclk_[a1_cpu_clks[0].pll], a1_cpu_clks[0].initial_hz, chip_id);

      break;
    }
    default:
      FDF_LOG(ERROR, "Unsupported SOC DID: %u", device_id);
      return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx::result<> AmlClock::Start() {
  // Initialize compat server.
  {
    compat::DeviceServer::BanjoConfig banjo_config{.default_proto_id = ZX_PROTOCOL_CLOCK_IMPL};
    banjo_config.callbacks[ZX_PROTOCOL_CLOCK_IMPL] = banjo_server_.callback();
    zx::result<> result = compat_server_.Initialize(
        incoming(), outgoing(), node_name(), kChildNodeName,
        compat::ForwardMetadata::Some({DEVICE_METADATA_CLOCK_IDS, DEVICE_METADATA_CLOCK_INIT}),
        std::move(banjo_config));
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to initialize compat server: %s", result.status_string());
      return result.take_error();
    }
  }

  // Get the platform device protocol and try to map all the MMIO regions.
  fdf::PDev pdev;
  {
    zx::result result = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
    if (result.is_error() || !result->is_valid()) {
      FDF_LOG(ERROR, "Failed to connect to platform device: %s", result.status_string());
      return result.take_error();
    }
    pdev = fdf::PDev{std::move(result.value())};
  }

  // All AML clocks have HIU and dosbus regs but only some support MSR regs.
  // Figure out which of the varieties we're dealing with.
  {
    zx::result hiu_mmio = pdev.MapMmio(kHiuMmio);
    if (hiu_mmio.is_error()) {
      FDF_LOG(ERROR, "Failed to map HIU mmio: %s", hiu_mmio.status_string());
      return hiu_mmio.take_error();
    }
    hiu_mmio_.emplace(std::move(hiu_mmio.value()));
  }

  {
    zx::result dosbus_mmio = pdev.MapMmio(kDosbusMmio);
    if (dosbus_mmio.is_error()) {
      FDF_LOG(ERROR, "Failed to map DOS mmio: %s", dosbus_mmio.status_string());
      return dosbus_mmio.take_error();
    }
    dosbus_mmio_.emplace(std::move(dosbus_mmio.value()));
  }

  // Use the Pdev Device Info to determine if we've been provided with two
  // MMIO regions.
  zx::result device_info = pdev.GetDeviceInfo();
  if (device_info.is_error()) {
    FDF_LOG(ERROR, "Failed to get device info: %s", device_info.status_string());
    return device_info.take_error();
  }

  if (device_info->vid == PDEV_VID_GENERIC && device_info->pid == PDEV_PID_GENERIC &&
      device_info->did == PDEV_DID_DEVICETREE_NODE) {
    // TODO(https://fxbug.dev/318736574) : Remove and rely only on GetDeviceInfo.
    zx::result board_info = pdev.GetBoardInfo();
    if (board_info.is_error()) {
      FDF_LOG(ERROR, "Failed to get board info: %s", board_info.status_string());
      return board_info.take_error();
    }

    if (board_info->vid == PDEV_VID_KHADAS) {
      switch (board_info->pid) {
        case PDEV_PID_VIM3:
          device_info->pid = PDEV_PID_AMLOGIC_A311D;
          device_info->did = PDEV_DID_AMLOGIC_G12B_CLK;
          break;
        default:
          FDF_LOG(ERROR, "Unsupported PID 0x%x for VID 0x%x", board_info->pid, board_info->vid);
          return zx::error(ZX_ERR_INVALID_ARGS);
      }
    } else {
      FDF_LOG(ERROR, "Unsupported VID 0x%x", board_info->vid);
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }

  if (device_info->mmio_count > kMsrMmio) {
    zx::result msr_mmio = pdev.MapMmio(kMsrMmio);
    if (msr_mmio.is_error()) {
      FDF_LOG(ERROR, "Failed to map MSR mmio: %s", msr_mmio.status_string());
      return msr_mmio.take_error();
    }
    msr_mmio_ = std::move(msr_mmio.value());
  }

  // For A1, this register is within cpuctrl mmio
  if (device_info->pid == PDEV_PID_AMLOGIC_A1 && device_info->mmio_count > kCpuCtrlMmio) {
    zx::result cpuctrl_mmio = pdev.MapMmio(kCpuCtrlMmio);
    if (cpuctrl_mmio.is_error()) {
      FDF_LOG(ERROR, "Failed to map cpuctrl mmio: %s", cpuctrl_mmio.status_string());
      return cpuctrl_mmio.take_error();
    }
    cpuctrl_mmio_ = std::move(cpuctrl_mmio.value());
  }

  zx_status_t status = PopulateRegisterBlocks(device_info->did, pdev);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to populate register blocks: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  status = InitChildNode();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to initialize child node: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

zx_status_t AmlClock::InitChildNode() {
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Failed to bind devfs connecter to dispatcher: %s", connector.status_string());
    return connector.status_value();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs_add_args{
      {.connector = std::move(connector.value())}};

  auto properties = {
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_clock::BIND_PROTOCOL_IMPL)};

  zx::result result =
      AddChild(kChildNodeName, devfs_add_args, properties, compat_server_.CreateOffers2());
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add device: %s", result.status_string());
    return result.status_value();
  }
  child_node_controller_.Bind(std::move(result.value()));

  return ZX_OK;
}

void AmlClock::DevfsConnect(fidl::ServerEnd<fuchsia_hardware_clock_measure::Measurer> request) {
  measurer_binding_group_.AddBinding(dispatcher(), std::move(request), this,
                                     fidl::kIgnoreBindingClosure);
}

zx_status_t AmlClock::ClkTogglePll(uint32_t id, const bool enable) {
  if (id >= pll_count_) {
    FDF_LOG(ERROR, "Invalid clkid: %d, pll count %zu", id, pll_count_);
    return ZX_ERR_INVALID_ARGS;
  }

  return pllclk_[id].Toggle(enable);
}

zx_status_t AmlClock::ClkToggle(uint32_t id, bool enable) {
  if (id >= gate_count_) {
    return ZX_ERR_INVALID_ARGS;
  }

  const meson_clk_gate_t* gate = &(gates_[id]);

  fbl::AutoLock al(&lock_);

  uint32_t enable_count = meson_gate_enable_count_[id];

  // For the sake of catching bugs, disabling a clock that has never
  // been enabled is a bug.
  ZX_ASSERT_MSG((enable == true || enable_count > 0),
                "Cannot disable already disabled clock. clkid = %u", id);

  // Update the refcounts.
  if (enable) {
    meson_gate_enable_count_[id]++;
  } else {
    ZX_ASSERT(enable_count > 0);
    meson_gate_enable_count_[id]--;
  }

  if (enable && meson_gate_enable_count_[id] == 1) {
    // Transition from 0 refs to 1.
    ClkToggleHw(gate, true);
  }

  if (!enable && meson_gate_enable_count_[id] == 0) {
    // Transition from 1 ref to 0.
    ClkToggleHw(gate, false);
  }

  return ZX_OK;
}

void AmlClock::ClkToggleHw(const meson_clk_gate_t* gate, bool enable) {
  uint32_t mask = gate->mask ? gate->mask : (1 << gate->bit);
  fdf::MmioBuffer* mmio;
  switch (gate->register_set) {
    case kMesonRegisterSetHiu:
      mmio = &hiu_mmio_.value();
      break;
    case kMesonRegisterSetDos:
      mmio = &dosbus_mmio_.value();
      break;
    default:
      ZX_PANIC("Unsupported register set: %d", gate->register_set);
  }

  if (enable) {
    mmio->SetBits32(mask, gate->reg);
  } else {
    mmio->ClearBits32(mask, gate->reg);
  }
}

zx_status_t AmlClock::ClockImplEnable(uint32_t id) {
  // Determine which clock type we're trying to control.
  aml_clk_common::aml_clk_type type = aml_clk_common::AmlClkType(id);
  const uint16_t clkid = aml_clk_common::AmlClkIndex(id);

  switch (type) {
    case aml_clk_common::aml_clk_type::kMesonGate:
      return ClkToggle(clkid, true);
    case aml_clk_common::aml_clk_type::kMesonPll:
      return ClkTogglePll(clkid, true);
    default:
      // Not a supported clock type?
      return ZX_ERR_NOT_SUPPORTED;
  }
}

zx_status_t AmlClock::ClockImplDisable(uint32_t id) {
  // Determine which clock type we're trying to control.
  aml_clk_common::aml_clk_type type = aml_clk_common::AmlClkType(id);
  const uint16_t clkid = aml_clk_common::AmlClkIndex(id);

  switch (type) {
    case aml_clk_common::aml_clk_type::kMesonGate:
      return ClkToggle(clkid, false);
    case aml_clk_common::aml_clk_type::kMesonPll:
      return ClkTogglePll(clkid, false);
    default:
      // Not a supported clock type?
      return ZX_ERR_NOT_SUPPORTED;
  };
}

zx_status_t AmlClock::ClockImplIsEnabled(uint32_t id, bool* out_enabled) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t AmlClock::ClockImplSetRate(uint32_t id, uint64_t hz) {
  FDF_LOG(TRACE, "%s: clk = %u, hz = %lu", __func__, id, hz);

  if (hz >= UINT32_MAX) {
    FDF_LOG(ERROR, "%s: requested rate exceeds uint32_max, clkid = %u, rate = %lu", __func__, id,
            hz);
    return ZX_ERR_INVALID_ARGS;
  }

  MesonRateClock* target_clock;
  zx_status_t st = GetMesonRateClock(id, &target_clock);
  if (st != ZX_OK) {
    return st;
  }

  return target_clock->SetRate(static_cast<uint32_t>(hz));
}

zx_status_t AmlClock::ClockImplQuerySupportedRate(uint32_t id, uint64_t max_rate,
                                                  uint64_t* out_best_rate) {
  FDF_LOG(TRACE, "%s: clkid = %u, max_rate = %lu", __func__, id, max_rate);

  if (out_best_rate == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  MesonRateClock* target_clock;
  zx_status_t st = GetMesonRateClock(id, &target_clock);
  if (st != ZX_OK) {
    return st;
  }

  return target_clock->QuerySupportedRate(max_rate, out_best_rate);
}

zx_status_t AmlClock::ClockImplGetRate(uint32_t id, uint64_t* out_current_rate) {
  FDF_LOG(TRACE, "%s: clkid = %u", __func__, id);

  if (out_current_rate == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  MesonRateClock* target_clock;
  zx_status_t st = GetMesonRateClock(id, &target_clock);
  if (st != ZX_OK) {
    return st;
  }

  return target_clock->GetRate(out_current_rate);
}

zx_status_t AmlClock::IsSupportedMux(uint32_t id, uint16_t supported_mask) {
  const uint16_t index = aml_clk_common::AmlClkIndex(id);
  const uint16_t type = static_cast<uint16_t>(aml_clk_common::AmlClkType(id));

  if ((type & supported_mask) == 0) {
    FDF_LOG(ERROR, "%s: Unsupported mux type for operation, clkid = %u", __func__, id);
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (!muxes_ || mux_count_ == 0) {
    FDF_LOG(ERROR, "%s: Platform does not have mux support.", __func__);
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (index >= mux_count_) {
    FDF_LOG(ERROR, "%s: Mux index out of bounds, count = %lu, idx = %u", __func__, mux_count_,
            index);
    return ZX_ERR_OUT_OF_RANGE;
  }

  return ZX_OK;
}

zx_status_t AmlClock::ClockImplSetInput(uint32_t id, uint32_t idx) {
  constexpr uint16_t kSupported = static_cast<uint16_t>(aml_clk_common::aml_clk_type::kMesonMux);
  zx_status_t st = IsSupportedMux(id, kSupported);
  if (st != ZX_OK) {
    return st;
  }

  const uint16_t index = aml_clk_common::AmlClkIndex(id);

  fbl::AutoLock al(&lock_);

  const meson_clk_mux_t& mux = muxes_[index];

  if (idx >= mux.n_inputs) {
    FDF_LOG(ERROR, "%s: mux input index out of bounds, max = %u, idx = %u.", __func__, mux.n_inputs,
            idx);
    return ZX_ERR_OUT_OF_RANGE;
  }

  uint32_t clkidx;
  if (mux.inputs) {
    clkidx = mux.inputs[idx];
  } else {
    clkidx = idx;
  }

  uint32_t val = hiu_mmio_->Read32(mux.reg);
  val &= ~(mux.mask << mux.shift);
  val |= (clkidx & mux.mask) << mux.shift;
  hiu_mmio_->Write32(val, mux.reg);

  return ZX_OK;
}

zx_status_t AmlClock::ClockImplGetNumInputs(uint32_t id, uint32_t* out_num_inputs) {
  constexpr uint16_t kSupported =
      (static_cast<uint16_t>(aml_clk_common::aml_clk_type::kMesonMux) |
       static_cast<uint16_t>(aml_clk_common::aml_clk_type::kMesonMuxRo));

  zx_status_t st = IsSupportedMux(id, kSupported);
  if (st != ZX_OK) {
    return st;
  }

  const uint16_t index = aml_clk_common::AmlClkIndex(id);

  const meson_clk_mux_t& mux = muxes_[index];

  *out_num_inputs = mux.n_inputs;

  return ZX_OK;
}

zx_status_t AmlClock::ClockImplGetInput(uint32_t id, uint32_t* out_input) {
  // Bitmask representing clock types that support this operation.
  constexpr uint16_t kSupported =
      (static_cast<uint16_t>(aml_clk_common::aml_clk_type::kMesonMux) |
       static_cast<uint16_t>(aml_clk_common::aml_clk_type::kMesonMuxRo));

  zx_status_t st = IsSupportedMux(id, kSupported);
  if (st != ZX_OK) {
    return st;
  }

  const uint16_t index = aml_clk_common::AmlClkIndex(id);

  const meson_clk_mux_t& mux = muxes_[index];

  const uint32_t result = (hiu_mmio_->Read32(mux.reg) >> mux.shift) & mux.mask;

  if (mux.inputs) {
    for (uint32_t i = 0; i < mux.n_inputs; i++) {
      if (result == mux.inputs[i]) {
        *out_input = i;
        return ZX_OK;
      }
    }
  }

  *out_input = result;
  return ZX_OK;
}

// Note: The clock index taken here are the index of clock
// from the clock table and not the clock_gates index.
// This API measures the clk frequency for clk.
// Following implementation is adopted from Amlogic SDK,
// there is absolutely no documentation.
zx_status_t AmlClock::ClkMeasureUtil(uint32_t id, uint64_t* clk_freq) {
  if (!msr_mmio_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Set the measurement gate to 64uS.
  uint32_t value = 64 - 1;
  msr_mmio_->Write32(value, clk_msr_offsets_.reg0_offset);
  // Disable continuous measurement.
  // Disable interrupts.
  value = MSR_CONT | MSR_INTR;
  // Clear the clock source.
  value |= MSR_CLK_SRC_MASK << MSR_CLK_SRC_SHIFT;
  msr_mmio_->ClearBits32(value, clk_msr_offsets_.reg0_offset);

  value = ((id << MSR_CLK_SRC_SHIFT) |  // Select the MUX.
           MSR_RUN |                    // Enable the clock.
           MSR_ENABLE);                 // Enable measuring.
  msr_mmio_->SetBits32(value, clk_msr_offsets_.reg0_offset);

  // Wait for the measurement to be done.
  for (uint32_t i = 0; i < MSR_WAIT_BUSY_RETRIES; i++) {
    value = msr_mmio_->Read32(clk_msr_offsets_.reg0_offset);
    if (value & MSR_BUSY) {
      // Wait a little bit before trying again.
      zx_nanosleep(zx_deadline_after(ZX_USEC(MSR_WAIT_BUSY_TIMEOUT_US)));
      continue;
    }
    // Disable measuring.
    msr_mmio_->ClearBits32(MSR_ENABLE, clk_msr_offsets_.reg0_offset);
    // Get the clock value.
    value = msr_mmio_->Read32(clk_msr_offsets_.reg2_offset);
    // Magic numbers, since lack of documentation.
    *clk_freq = (((value + 31) & MSR_VAL_MASK) / 64);
    return ZX_OK;
  }
  return ZX_ERR_TIMED_OUT;
}

void AmlClock::Measure(MeasureRequestView request, MeasureCompleter::Sync& completer) {
  fuchsia_hardware_clock_measure::wire::FrequencyInfo info;
  if (request->clock >= clk_table_count_) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  std::string name = clk_table_[request->clock];
  if (name.length() >= fuchsia_hardware_clock_measure::wire::kMaxNameLen) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  info.name = fidl::StringView::FromExternal(name);
  zx_status_t status = ClkMeasureUtil(request->clock, &info.frequency);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  completer.ReplySuccess(info);
}

void AmlClock::GetCount(GetCountCompleter::Sync& completer) {
  completer.Reply(static_cast<uint32_t>(clk_table_count_));
}

void AmlClock::Stop() {
  hiu_mmio_.reset();

  if (msr_mmio_) {
    msr_mmio_->reset();
  }
}

zx_status_t AmlClock::GetMesonRateClock(const uint32_t id, MesonRateClock** out) {
  aml_clk_common::aml_clk_type type = aml_clk_common::AmlClkType(id);
  const uint16_t clkid = aml_clk_common::AmlClkIndex(id);

  switch (type) {
    case aml_clk_common::aml_clk_type::kMesonPll:
      if (clkid >= pll_count_) {
        FDF_LOG(ERROR, "%s: HIU PLL out of range, clkid = %hu.", __func__, clkid);
        return ZX_ERR_INVALID_ARGS;
      }

      *out = &pllclk_[clkid];
      return ZX_OK;
    case aml_clk_common::aml_clk_type::kMesonCpuClk:
      if (clkid >= cpu_clks_.size()) {
        FDF_LOG(ERROR, "%s: cpu clk out of range, clkid = %hu.", __func__, clkid);
        return ZX_ERR_INVALID_ARGS;
      }

      *out = &cpu_clks_[clkid];
      return ZX_OK;
    default:
      FDF_LOG(ERROR, "%s: Unsupported clock type, type = 0x%hx\n", __func__,
              static_cast<unsigned short>(type));
      return ZX_ERR_NOT_SUPPORTED;
  }

  __UNREACHABLE;
}

void AmlClock::InitHiu() {
  pllclk_.reserve(pll_count_);
  s905d2_hiu_init_etc(&*hiudev_, hiu_mmio_->View(0));
  for (unsigned int pllnum = 0; pllnum < pll_count_; pllnum++) {
    const hhi_plls_t pll = static_cast<hhi_plls_t>(pllnum);
    pllclk_.emplace_back(pll, &*hiudev_);
    pllclk_[pllnum].Init();
  }
}

void AmlClock::InitHiuA5() {
  pllclk_.reserve(pll_count_);
  for (unsigned int pllnum = 0; pllnum < pll_count_; pllnum++) {
    auto plldev = a5::CreatePllDevice(&dosbus_mmio_.value(), pllnum);
    pllclk_.emplace_back(std::move(plldev));
    pllclk_[pllnum].Init();
  }
}

void AmlClock::InitHiuA1() {
  pllclk_.reserve(pll_count_);
  for (unsigned int pllnum = 0; pllnum < pll_count_; pllnum++) {
    auto plldev = a1::CreatePllDevice(&dosbus_mmio_.value(), pllnum);
    pllclk_.emplace_back(std::move(plldev));
    pllclk_[pllnum].Init();
  }
}

}  // namespace amlogic_clock

FUCHSIA_DRIVER_EXPORT(amlogic_clock::AmlClock);
