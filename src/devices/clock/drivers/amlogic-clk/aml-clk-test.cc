// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-clk.h"

#include <lib/ddk/platform-defs.h>
#include <lib/mmio-ptr/fake.h>
#include <lib/mmio/mmio-buffer.h>

#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-meson/aml-clk-common.h>
#include <soc/aml-meson/axg-clk.h>
#include <soc/aml-meson/g12a-clk.h>
#include <soc/aml-meson/sm1-clk.h>
#include <soc/aml-s905d2/s905d2-hw.h>
#include <soc/aml-s905d3/s905d3-hw.h>
#include <soc/aml-s912/s912-hw.h>
#include <zxtest/zxtest.h>

#include "aml-axg-blocks.h"
#include "aml-g12a-blocks.h"
#include "aml-g12b-blocks.h"
#include "aml-sm1-blocks.h"
#include "src/devices/lib/mmio/test-helper.h"

namespace amlogic_clock {

namespace {
constexpr uint64_t kilohertz(uint64_t hz) { return hz * 1000; }
constexpr uint64_t megahertz(uint64_t hz) { return kilohertz(hz) * 1000; }
constexpr uint64_t gigahertz(uint64_t hz) { return megahertz(hz) * 1000; }

constexpr uint32_t kCpuClkSupportedFrequencies[] = {
    100'000'000,   250'000'000,   500'000'000,   667'000'000,   1'000'000'000, 1'200'000'000,
    1'398'000'000, 1'512'000'000, 1'608'000'000, 1'704'000'000, 1'896'000'000,
};

}  // namespace

class AmlClockTest : public AmlClock {
 public:
  AmlClockTest(fdf::MmioBuffer mmio_buffer, fdf::MmioBuffer dosbus_buffer, uint32_t did)
      : AmlClock(nullptr, std::move(mmio_buffer), std::move(dosbus_buffer), std::nullopt,
                 std::nullopt, did) {}
  ~AmlClockTest() = default;
};

bool MmioMemcmp(fdf::MmioBuffer& actual, std::unique_ptr<uint8_t[]>& expected) {
  for (size_t i = 0; i < actual.get_size(); i++) {
    if (actual.Read8(i) != expected[i]) {
      return true;
    }
  }
  return false;
}

std::tuple<fdf::MmioView, fdf::MmioBuffer> MakeDosbusMmio() {
  auto dos_buffer = fdf_testing::CreateMmioBuffer(S912_DOS_LENGTH);
  return std::make_tuple(dos_buffer.View(0), std::move(dos_buffer));
}

TEST(ClkTestAml, AxgEnableDisableAll) {
  auto expected = std::make_unique<uint8_t[]>(S912_HIU_LENGTH);
  auto buffer = fdf_testing::CreateMmioBuffer(S912_HIU_LENGTH);
  auto actual = buffer.View(0);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();

  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_AXG_CLK);

  // Initialization sets a bunch of registers that we don't care about, so we
  // can reset the array to a clean slate.
  memset(expected.get(), 0, S912_HIU_LENGTH);

  EXPECT_EQ(MmioMemcmp(actual, expected), 0);
  constexpr uint16_t kClkStart = 0;
  constexpr uint16_t kClkEnd = static_cast<uint16_t>(axg_clk::CLK_AXG_COUNT);

  for (uint16_t i = kClkStart; i < kClkEnd; ++i) {
    if (axg_clk_gates[i].register_set != kMesonRegisterSetHiu)
      continue;
    const uint32_t reg = axg_clk_gates[i].reg;
    const uint32_t bit = (1u << axg_clk_gates[i].bit);
    uint32_t* ptr = reinterpret_cast<uint32_t*>(&expected[reg]);
    (*ptr) |= bit;

    const uint32_t clk_i = aml_clk_common::AmlClkId(i, aml_clk_common::aml_clk_type::kMesonGate);
    zx_status_t st = clk.ClockImplEnable(clk_i);
    EXPECT_OK(st);
  }

  EXPECT_EQ(MmioMemcmp(actual, expected), 0);

  for (uint16_t i = kClkStart; i < kClkEnd; ++i) {
    if (axg_clk_gates[i].register_set != kMesonRegisterSetHiu)
      continue;
    const uint32_t reg = axg_clk_gates[i].reg;
    const uint32_t bit = (1u << axg_clk_gates[i].bit);
    uint32_t* ptr = reinterpret_cast<uint32_t*>(&expected[reg]);
    (*ptr) &= ~(bit);

    const uint32_t clk_i = aml_clk_common::AmlClkId(i, aml_clk_common::aml_clk_type::kMesonGate);
    zx_status_t st = clk.ClockImplDisable(clk_i);
    EXPECT_OK(st);
  }

  EXPECT_EQ(MmioMemcmp(actual, expected), 0);
}

TEST(ClkTestAml, G12aEnableDisableAll) {
  auto expected = std::make_unique<uint8_t[]>(S912_HIU_LENGTH);
  auto buffer = fdf_testing::CreateMmioBuffer(S912_HIU_LENGTH);
  auto actual = buffer.View(0);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();
  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_G12A_CLK);

  // Initialization sets a bunch of registers that we don't care about, so we
  // can reset the array to a clean slate.
  memset(expected.get(), 0, S905D2_HIU_LENGTH);

  EXPECT_EQ(MmioMemcmp(actual, expected), 0);

  constexpr uint16_t kClkStart = 0;
  constexpr uint16_t kClkEnd = static_cast<uint16_t>(g12a_clk::CLK_G12A_COUNT);

  for (uint16_t i = kClkStart; i < kClkEnd; ++i) {
    if (g12a_clk_gates[i].register_set != kMesonRegisterSetHiu)
      continue;
    const uint32_t reg = g12a_clk_gates[i].reg;
    const uint32_t bit = (1u << g12a_clk_gates[i].bit);
    uint32_t* ptr = reinterpret_cast<uint32_t*>(&expected[reg]);
    (*ptr) |= bit;

    const uint32_t clk_i = aml_clk_common::AmlClkId(i, aml_clk_common::aml_clk_type::kMesonGate);
    zx_status_t st = clk.ClockImplEnable(clk_i);
    EXPECT_OK(st);
  }

  EXPECT_EQ(MmioMemcmp(actual, expected), 0);

  for (uint16_t i = kClkStart; i < kClkEnd; ++i) {
    if (g12a_clk_gates[i].register_set != kMesonRegisterSetHiu)
      continue;
    const uint32_t reg = g12a_clk_gates[i].reg;
    const uint32_t bit = (1u << g12a_clk_gates[i].bit);
    uint32_t* ptr = reinterpret_cast<uint32_t*>(&expected[reg]);
    (*ptr) &= ~(bit);

    const uint32_t clk_i = aml_clk_common::AmlClkId(i, aml_clk_common::aml_clk_type::kMesonGate);
    zx_status_t st = clk.ClockImplDisable(clk_i);
    EXPECT_OK(st);
  }

  EXPECT_EQ(MmioMemcmp(actual, expected), 0);
}

TEST(ClkTestAml, Sm1EnableDisableAll) {
  auto expected = std::make_unique<uint8_t[]>(S905D3_HIU_LENGTH);
  auto buffer = fdf_testing::CreateMmioBuffer(S905D3_HIU_LENGTH);
  auto actual = buffer.View(0);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();
  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_SM1_CLK);

  // Initialization sets a bunch of registers that we don't care about, so we
  // can reset the array to a clean slate.
  memset(expected.get(), 0, S905D3_HIU_LENGTH);

  EXPECT_EQ(MmioMemcmp(actual, expected), 0);

  constexpr uint16_t kClkStart = 0;
  constexpr uint16_t kClkEnd = static_cast<uint16_t>(sm1_clk::CLK_SM1_GATE_COUNT);

  for (uint16_t i = kClkStart; i < kClkEnd; ++i) {
    if (sm1_clk_gates[i].register_set != kMesonRegisterSetHiu)
      continue;
    const uint32_t reg = sm1_clk_gates[i].reg;
    const uint32_t bit = (1u << sm1_clk_gates[i].bit);
    uint32_t* ptr = reinterpret_cast<uint32_t*>(&expected[reg]);
    (*ptr) |= bit;

    const uint32_t clk_i = aml_clk_common::AmlClkId(i, aml_clk_common::aml_clk_type::kMesonGate);
    zx_status_t st = clk.ClockImplEnable(clk_i);
    EXPECT_OK(st);
  }

  EXPECT_EQ(MmioMemcmp(actual, expected), 0);

  for (uint16_t i = kClkStart; i < kClkEnd; ++i) {
    if (sm1_clk_gates[i].register_set != kMesonRegisterSetHiu)
      continue;
    const uint32_t reg = sm1_clk_gates[i].reg;
    const uint32_t bit = (1u << sm1_clk_gates[i].bit);
    uint32_t* ptr = reinterpret_cast<uint32_t*>(&expected[reg]);
    (*ptr) &= ~(bit);

    const uint32_t clk_i = aml_clk_common::AmlClkId(i, aml_clk_common::aml_clk_type::kMesonGate);
    zx_status_t st = clk.ClockImplDisable(clk_i);
    EXPECT_OK(st);
  }

  EXPECT_EQ(MmioMemcmp(actual, expected), 0);
}

TEST(ClkTestAml, G12aEnableDos) {
  auto buffer = fdf_testing::CreateMmioBuffer(S905D2_HIU_LENGTH);
  auto actual = buffer.View(0);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();
  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_G12A_CLK);

  zx_status_t st = clk.ClockImplEnable(g12a_clk::CLK_DOS_GCLK_VDEC);
  EXPECT_OK(st);

  EXPECT_EQ(0x3ff, dos_data.Read32(0x3f01 * sizeof(uint32_t)));
}

TEST(ClkTestAml, G12bEnableAudio) {
  auto buffer = fdf_testing::CreateMmioBuffer(A311D_HIU_LENGTH);
  auto actual = buffer.View(0);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();
  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_G12B_CLK);

  zx_status_t st = clk.ClockImplEnable(g12b_clk::G12B_CLK_AUDIO);
  EXPECT_OK(st);

  constexpr uint32_t kHhiGclkMpeg1Offset = 0x51;
  EXPECT_EQ(0x1, actual.Read32(kHhiGclkMpeg1Offset * sizeof(uint32_t)));
}

TEST(ClkTestAml, G12bEnableEmmcC) {
  auto buffer = fdf_testing::CreateMmioBuffer(A311D_HIU_LENGTH);
  auto actual = buffer.View(0);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();
  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_G12B_CLK);

  zx_status_t st = clk.ClockImplEnable(g12b_clk::G12B_CLK_EMMC_C);
  EXPECT_OK(st);

  constexpr uint32_t kHhiGclkMpeg0Offset = 0x50;
  EXPECT_EQ(0x1 << 26, actual.Read32(kHhiGclkMpeg0Offset * sizeof(uint32_t)));
}

static void TestPlls(const uint32_t did) {
  auto ignored = std::make_unique<uint8_t[]>(S905D2_HIU_LENGTH);
  auto buffer = fdf_testing::CreateMmioBuffer(S905D2_HIU_LENGTH);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();
  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), did);

  constexpr uint16_t kPllStart = 0;
  constexpr uint16_t kPllEnd = HIU_PLL_COUNT;

  for (uint16_t i = kPllStart; i < kPllEnd; i++) {
    const uint32_t clkid = aml_clk_common::AmlClkId(i, aml_clk_common::aml_clk_type::kMesonPll);

    zx_status_t st;
    uint64_t best_supported_rate;
    constexpr uint64_t kMaxRateHz = gigahertz(1);
    st = clk.ClockImplQuerySupportedRate(clkid, kMaxRateHz, &best_supported_rate);
    EXPECT_OK(st);

    EXPECT_LE(best_supported_rate, kMaxRateHz);

    st = clk.ClockImplSetRate(clkid, best_supported_rate);
    EXPECT_OK(st);
  }
}

TEST(ClkTestAml, G12aSetRate) { TestPlls(PDEV_DID_AMLOGIC_G12A_CLK); }

TEST(ClkTestAml, G12bSetRate) { TestPlls(PDEV_DID_AMLOGIC_G12B_CLK); }

TEST(ClkTestAml, Sm1MuxRo) {
  auto buffer = fdf_testing::CreateMmioBuffer(S905D3_HIU_LENGTH);
  auto regs = buffer.View(0);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();
  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_SM1_CLK);

  // Ensure that SetInput fails for RO muxes.
  zx_status_t st = clk.ClockImplSetInput(sm1_clk::CLK_MPEG_CLK_SEL, 0);
  EXPECT_NOT_OK(st);

  // Make sure we can read the number of parents.
  uint32_t out_num_inputs = 0;
  st = clk.ClockImplGetNumInputs(sm1_clk::CLK_MPEG_CLK_SEL, &out_num_inputs);
  EXPECT_OK(st);
  EXPECT_GT(out_num_inputs, 0);

  // Make sure that we can read the current parent of the mux.
  uint32_t out_input = UINT32_MAX;
  st = clk.ClockImplGetInput(sm1_clk::CLK_MPEG_CLK_SEL, &out_input);
  EXPECT_OK(st);
  EXPECT_NE(out_input, UINT32_MAX);

  // Also ensure that we didn't whack any registers for a read-only mux.
  for (size_t i = 0; i < S905D3_HIU_LENGTH; i++) {
    EXPECT_EQ(0, regs.Read8(i));
  }
}

TEST(ClkTestAml, Sm1Mux) {
  constexpr uint32_t kTestMux = sm1_clk::CLK_CTS_VIPNANOQ_AXI_CLK_MUX;

  constexpr uint16_t kTestMuxIdx = aml_clk_common::AmlClkIndex(kTestMux);

  const meson_clk_mux_t& test_mux = sm1_muxes[kTestMuxIdx];

  auto buffer = fdf_testing::CreateMmioBuffer(S905D3_HIU_LENGTH);
  auto regs = buffer.View(0);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();
  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_SM1_CLK);

  const uint32_t newParentIdx = test_mux.n_inputs - 1;
  zx_status_t st = clk.ClockImplSetInput(kTestMux, newParentIdx);
  EXPECT_OK(st);

  const uint32_t actual_regval = regs.Read32((test_mux.reg >> 2) * sizeof(uint32_t));
  const uint32_t expected_regval = (newParentIdx & test_mux.mask) << test_mux.shift;
  EXPECT_EQ(expected_regval, actual_regval);

  // Make sure we can read the number of parents.
  uint32_t out_num_inputs = 0;
  st = clk.ClockImplGetNumInputs(kTestMux, &out_num_inputs);
  EXPECT_OK(st);
  EXPECT_EQ(out_num_inputs, test_mux.n_inputs);

  // Make sure that we can read the current parent of the mux.
  uint32_t out_input = UINT32_MAX;
  st = clk.ClockImplGetInput(kTestMux, &out_input);
  EXPECT_OK(st);
  EXPECT_EQ(out_input, newParentIdx);
}

TEST(ClkTestAml, TestCpuClkSetRate) {
  constexpr uint32_t kTestCpuClk = g12a_clk::CLK_SYS_CPU_CLK;

  auto regs = std::make_unique<uint8_t[]>(S905D2_HIU_LENGTH);
  auto buffer = fdf_testing::CreateMmioBuffer(S905D2_HIU_LENGTH);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();

  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_G12A_CLK);

  zx_status_t st;
  for (size_t i = 0; i < std::size(kCpuClkSupportedFrequencies); i++) {
    st = clk.ClockImplSetRate(kTestCpuClk, kCpuClkSupportedFrequencies[i]);
    EXPECT_OK(st);

    st = clk.ClockImplSetRate(kTestCpuClk, kCpuClkSupportedFrequencies[i] + 1);
    EXPECT_NOT_OK(st);
  }
}

TEST(ClkTestAml, TestCpuClkQuerySupportedRates) {
  constexpr uint32_t kTestCpuClk = g12a_clk::CLK_SYS_CPU_CLK;
  constexpr uint32_t kJustOver1GHz = gigahertz(1) + 1;

  auto regs = std::make_unique<uint8_t[]>(S905D2_HIU_LENGTH);
  auto buffer = fdf_testing::CreateMmioBuffer(S905D2_HIU_LENGTH);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();

  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_G12A_CLK);

  uint64_t rate;
  zx_status_t st = clk.ClockImplQuerySupportedRate(kTestCpuClk, kJustOver1GHz, &rate);
  EXPECT_OK(st);
  EXPECT_EQ(rate, gigahertz(1));
}

TEST(ClkTestAml, TestCpuClkGetRate) {
  constexpr uint32_t kTestCpuClk = g12a_clk::CLK_SYS_CPU_CLK;
  constexpr uint32_t kOneGHz = gigahertz(1);

  auto regs = std::make_unique<uint8_t[]>(S905D2_HIU_LENGTH);
  auto buffer = fdf_testing::CreateMmioBuffer(S905D2_HIU_LENGTH);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();

  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_G12A_CLK);
  zx_status_t st;

  st = clk.ClockImplSetRate(kTestCpuClk, kOneGHz);
  EXPECT_OK(st);

  uint64_t rate;
  st = clk.ClockImplGetRate(kTestCpuClk, &rate);
  EXPECT_OK(st);
  EXPECT_EQ(rate, kOneGHz);
}

TEST(ClkTestAml, TestCpuClkG12b) {
  constexpr uint32_t kTestCpuBigClk = g12b_clk::CLK_SYS_CPU_BIG_CLK;
  constexpr uint32_t kTestCpuLittleClk = g12b_clk::CLK_SYS_CPU_LITTLE_CLK;
  constexpr uint32_t kBigClockTestFreq = gigahertz(1);
  constexpr uint32_t kLittleClockTestFreq = megahertz(1800);

  auto regs = std::make_unique<uint8_t[]>(S905D2_HIU_LENGTH);
  auto buffer = fdf_testing::CreateMmioBuffer(S905D2_HIU_LENGTH);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();

  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_G12B_CLK);
  zx_status_t st;

  st = clk.ClockImplSetRate(kTestCpuBigClk, kBigClockTestFreq);
  EXPECT_OK(st);

  uint64_t rate;
  st = clk.ClockImplGetRate(kTestCpuBigClk, &rate);
  EXPECT_OK(st);
  EXPECT_EQ(rate, kBigClockTestFreq);

  st = clk.ClockImplSetRate(kTestCpuLittleClk, kLittleClockTestFreq);
  EXPECT_OK(st);

  st = clk.ClockImplGetRate(kTestCpuLittleClk, &rate);
  EXPECT_OK(st);
  EXPECT_EQ(rate, kLittleClockTestFreq);
}

TEST(ClkTestAml, DisableRefZero) {
  // Attempts to disable a clock that has never been enabled.
  // Confirm that this is fatal.
  auto expected = std::make_unique<uint8_t[]>(S905D2_HIU_LENGTH);
  auto buffer = fdf_testing::CreateMmioBuffer(S905D2_HIU_LENGTH);
  auto actual = buffer.View(0);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();
  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_G12A_CLK);

  // Initialization sets a bunch of registers that we don't care about, so we
  // can reset the array to a clean slate.
  memset(expected.get(), 0, S905D2_HIU_LENGTH);

  EXPECT_EQ(MmioMemcmp(actual, expected), 0);

  constexpr uint16_t kTestClock = 0;
  const uint32_t clk_id =
      aml_clk_common::AmlClkId(kTestClock, aml_clk_common::aml_clk_type::kMesonGate);

  ASSERT_DEATH(([&clk]() { clk.ClockImplDisable(clk_id); }),
               "Failed to crash when trying to disable already disabled clock.");
}

TEST(ClkTestAml, EnableDisableRefCount) {
  zx_status_t st;

  auto buffer = fdf_testing::CreateMmioBuffer(S905D2_HIU_LENGTH);
  auto actual = buffer.View(0);

  auto [dos_data, dos_buffer] = MakeDosbusMmio();
  AmlClockTest clk(std::move(buffer), std::move(dos_buffer), PDEV_DID_AMLOGIC_G12A_CLK);

  // Initialization sets a bunch of registers that we don't care about, so we
  // can reset the array to a clean slate.
  constexpr uint16_t kTestClock = 0;
  constexpr uint32_t kTestClockId =
      aml_clk_common::AmlClkId(kTestClock, aml_clk_common::aml_clk_type::kMesonGate);
  constexpr uint32_t reg = g12a_clk_gates[kTestClock].reg;
  constexpr uint32_t bit = (1u << g12a_clk_gates[kTestClock].bit);

  // Enable the test clock and verify that the register was written.
  st = clk.ClockImplEnable(kTestClockId);
  EXPECT_OK(st);
  EXPECT_EQ(actual.Read32(reg), bit);

  // Zero out the register and enable the clock again. Since the driver
  // should already think the clock is enabled, this should be a no-op.
  actual.Write32(0, reg);
  st = clk.ClockImplEnable(kTestClockId);
  EXPECT_OK(st);
  EXPECT_EQ(actual.Read32(reg), 0);  // Make sure the driver didn't touch the register.

  // This time we fill the registers with Fs and try disabling the clock. Since
  // it's been enabled twice, the first disable should only drop a ref rather than
  // actually disabling clock hardware.
  actual.Write32(0xffffffff, reg);
  st = clk.ClockImplDisable(kTestClockId);
  EXPECT_OK(st);
  EXPECT_EQ(actual.Read32(reg), 0xffffffff);  // Make sure the driver didn't touch the register.

  // The second call to disable should actually disable the clock hardware.
  actual.Write32(0xffffffff, reg);
  st = clk.ClockImplDisable(kTestClockId);
  EXPECT_OK(st);
  EXPECT_EQ(actual.Read32(reg),
            (0xffffffff & (~bit)));  // Make sure the driver actually disabled the hardware.
}

}  // namespace amlogic_clock
