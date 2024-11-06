// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_CLOCK_DRIVERS_AMLOGIC_CLK_AML_CLK_H_
#define SRC_DEVICES_CLOCK_DRIVERS_AMLOGIC_CLK_AML_CLK_H_

#include <fidl/fuchsia.hardware.clock.measure/cpp/wire.h>
#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <fuchsia/hardware/clockimpl/cpp/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/io-buffer.h>
#include <lib/driver/platform-device/cpp/pdev.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <zircon/syscalls/smc.h>

#include <ddktl/device.h>
#include <fbl/array.h>
#include <fbl/mutex.h>
#include <hwreg/mmio.h>
#include <soc/aml-a1/a1-hiu.h>
#include <soc/aml-a5/a5-hiu.h>
#include <soc/aml-s905d2/s905d2-hiu.h>

#include "aml-clk-blocks.h"
#include "meson-cpu-clock.h"
#include "meson-pll-clock.h"
#include "meson-rate-clock.h"

namespace amlogic_clock {

class AmlClock;
using DeviceType = ddk::Device<AmlClock, ddk::Unbindable,
                               ddk::Messageable<fuchsia_hardware_clock_measure::Measurer>::Mixin>;

class AmlClock : public DeviceType, public ddk::ClockImplProtocol<AmlClock, ddk::base_protocol> {
 public:
  // MMIO Indexes
  static constexpr uint32_t kHiuMmio = 0;
  static constexpr uint32_t kDosbusMmio = 1;
  static constexpr uint32_t kMsrMmio = 2;
  static constexpr uint32_t kCpuCtrlMmio = 3;

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(AmlClock);
  AmlClock(zx_device_t* device, fdf::MmioBuffer hiu_mmio, fdf::MmioBuffer dosbus_mmio,
           std::optional<fdf::MmioBuffer> msr_mmio, std::optional<fdf::MmioBuffer> cpuctrl_mmio);
  ~AmlClock() override = default;

  // Performs the object initialization.
  static zx_status_t Bind(void* ctx, zx_device_t* device);

  // CLK protocol implementation.
  zx_status_t ClockImplEnable(uint32_t id);
  zx_status_t ClockImplDisable(uint32_t id);
  zx_status_t ClockImplIsEnabled(uint32_t id, bool* out_enabled);

  zx_status_t ClockImplSetRate(uint32_t id, uint64_t hz);
  zx_status_t ClockImplQuerySupportedRate(uint32_t id, uint64_t max_rate, uint64_t* out_best_rate);
  zx_status_t ClockImplGetRate(uint32_t id, uint64_t* out_current_rate);

  zx_status_t ClockImplSetInput(uint32_t id, uint32_t idx);
  zx_status_t ClockImplGetNumInputs(uint32_t id, uint32_t* out_num_inputs);
  zx_status_t ClockImplGetInput(uint32_t id, uint32_t* out_input);

  zx_status_t Init(uint32_t device_id, fdf::PDev& pdev);

  // CLK FIDL implementation.
  void Measure(MeasureRequestView request, MeasureCompleter::Sync& completer) override;
  void GetCount(GetCountCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_clock_measure::Measurer> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    zxlogf(ERROR, "Unexpected Clock FIDL call: 0x%lx", metadata.method_ordinal);
  }

  // Device protocol implementation.
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  void ShutDown();

 private:
  // Toggle clocks enable bit.
  zx_status_t ClkToggle(uint32_t id, bool enable);
  void ClkToggleHw(const meson_clk_gate_t* gate, bool enable) __TA_REQUIRES(lock_);

  // Clock measure helper API.
  zx_status_t ClkMeasureUtil(uint32_t id, uint64_t* clk_freq);

  // Toggle enable bit for PLL clocks.
  zx_status_t ClkTogglePll(uint32_t id, bool enable);

  // Checks the preconditions for SetInput, GetNumInputs and GetInput and
  // returns ZX_OK if the preconditions are met.
  zx_status_t IsSupportedMux(uint32_t id, uint16_t supported_mask);

  // Find the MesonRateClock that corresponds to clk. If ZX_OK is returned
  // `out` is populated with a pointer to the target clock.
  zx_status_t GetMesonRateClock(uint32_t id, MesonRateClock** out);

  void InitHiu();

  void InitHiuA5();

  void InitHiuA1();

  // IO MMIO
  fdf::MmioBuffer hiu_mmio_;
  fdf::MmioBuffer dosbus_mmio_;
  std::optional<fdf::MmioBuffer> msr_mmio_;
  std::optional<fdf::MmioBuffer> cpuctrl_mmio_;
  // Protects clock gate registers.
  // Clock gates.
  fbl::Mutex lock_;
  const meson_clk_gate_t* gates_ = nullptr;
  size_t gate_count_ = 0;
  std::vector<uint32_t> meson_gate_enable_count_;

  // Clock muxes.
  const meson_clk_mux_t* muxes_ = nullptr;
  size_t mux_count_ = 0;

  // Cpu Clocks.
  std::vector<MesonCpuClock> cpu_clks_;

  std::optional<fdf::MmioBuffer> hiudev_;
  std::vector<MesonPllClock> pllclk_;
  size_t pll_count_ = HIU_PLL_COUNT;

  // Clock Table
  const char* const* clk_table_ = nullptr;
  size_t clk_table_count_ = 0;
  // MSR_CLK offsets/
  meson_clk_msr_t clk_msr_offsets_;
};

}  // namespace amlogic_clock

#endif  // SRC_DEVICES_CLOCK_DRIVERS_AMLOGIC_CLK_AML_CLK_H_
