// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "as370-clk.h"

#include <mock-mmio-reg/mock-mmio-reg.h>
#include <soc/as370/as370-clk.h>
#include <soc/as370/as370-hw.h>

namespace clk {

class As370ClkTest : public As370Clk {
 public:
  As370ClkTest(ddk_mock::MockMmioRegRegion& global_mmio, ddk_mock::MockMmioRegRegion& audio_mmio,
               ddk_mock::MockMmioRegRegion& cpu_mmio)
      : As370Clk(nullptr, ddk::MmioBuffer(global_mmio.GetMmioBuffer()),
                 ddk::MmioBuffer(audio_mmio.GetMmioBuffer()),
                 ddk::MmioBuffer(cpu_mmio.GetMmioBuffer())) {}
};

TEST(ClkSynTest, AvpllClkEnable) {
  ddk_mock::MockMmioRegRegion global_region(4, as370::kGlobalSize / 4);
  ddk_mock::MockMmioRegRegion audio_region(4, as370::kAudioGlobalSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(global_region, audio_region, unused);

  global_region[0x0530].ExpectRead(0x00000000).ExpectWrite(0x00000001);  // Enable AVIO clock.
  global_region[0x0088].ExpectRead(0xffffffff).ExpectWrite(0xfffffffe);  // Not sysPll power down.
  audio_region[0x0044].ExpectRead(0x00000000).ExpectWrite(0x00000004);   // Enable AVPLL.
  audio_region[0x0000].ExpectRead(0x00000000).ExpectWrite(0x00000020);   // Enable AVPLL Clock.

  EXPECT_OK(test.ClockImplEnable(0));

  global_region.VerifyAll();
  audio_region.VerifyAll();
}

TEST(ClkSynTest, AvpllClkDisable) {
  ddk_mock::MockMmioRegRegion global_region(4, as370::kGlobalSize / 4);
  ddk_mock::MockMmioRegRegion audio_region(4, as370::kAudioGlobalSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(global_region, audio_region, unused);

  audio_region[0x0044].ExpectRead(0xffffffff).ExpectWrite(0xfffffffb);  // Disable AVPLL.
  audio_region[0x0000].ExpectRead(0xffffffff).ExpectWrite(0xffffffdf);  // Disable AVPLL Clock.

  EXPECT_OK(test.ClockImplDisable(0));

  global_region.VerifyAll();
  audio_region.VerifyAll();
}

TEST(ClkSynTest, AvpllClkDisablePll1) {
  ddk_mock::MockMmioRegRegion global_region(4, as370::kGlobalSize / 4);
  ddk_mock::MockMmioRegRegion audio_region(4, as370::kAudioGlobalSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(global_region, audio_region, unused);

  audio_region[0x0044].ExpectRead(0xffffffff).ExpectWrite(0xfffffff7);  // Disable AVPLL 1.
  audio_region[0x0020].ExpectRead(0xffffffff).ExpectWrite(0xffffffdf);  // Disable AVPLL Clock.

  EXPECT_OK(test.ClockImplDisable(1));

  global_region.VerifyAll();
  audio_region.VerifyAll();
}

TEST(ClkSynTest, AvpllSetRateBad) {
  ddk_mock::MockMmioRegRegion global_region(4, as370::kGlobalSize / 4);
  ddk_mock::MockMmioRegRegion audio_region(4, as370::kAudioGlobalSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(global_region, audio_region, unused);

  EXPECT_NOT_OK(test.ClockImplSetRate(0, 800'000'001));  // Too high.
}

TEST(ClkSynTest, AvpllSetRateGood) {
  ddk_mock::MockMmioRegRegion global_region(4, as370::kGlobalSize / 4);
  ddk_mock::MockMmioRegRegion audio_region(4, as370::kAudioGlobalSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(global_region, audio_region, unused);

  audio_region[0x0044].ExpectRead(0xffffffff).ExpectWrite(0xfffffffb);  // Clock disable.
  audio_region[0x0018].ExpectRead(0x00000000).ExpectWrite(0x00000001);  // Bypass.
  audio_region[0x0014].ExpectRead(0x00000000).ExpectWrite(0x01000000);  // Power down DP.

  audio_region[0x0008].ExpectRead(0x00000000).ExpectWrite(0x0000e004);  // dn 224 dm 1.
  audio_region[0x0014].ExpectRead(0x00000000).ExpectWrite(0x0e000000);  // dp 7.

  audio_region[0x0014].ExpectRead(0xffffffff).ExpectWrite(0xfeffffff);  // Power up DP.
  audio_region[0x0018].ExpectRead(0xffffffff).ExpectWrite(0xfffffffe);  // Remove bypass.
  audio_region[0x0044].ExpectRead(0x00000000).ExpectWrite(0x00000004);  // Clock enable.

  EXPECT_OK(test.ClockImplSetRate(0, 800'000'000));

  audio_region.VerifyAll();
}

TEST(ClkSynTest, AvpllSetRateFractionalFor48KHz) {
  ddk_mock::MockMmioRegRegion global_region(4, as370::kGlobalSize / 4);
  ddk_mock::MockMmioRegRegion audio_region(4, as370::kAudioGlobalSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(global_region, audio_region, unused);

  audio_region[0x0044].ExpectRead(0xffffffff).ExpectWrite(0xfffffffb);  // Clock disable.
  audio_region[0x0018].ExpectRead(0x00000000).ExpectWrite(0x00000001);  // Bypass.
  audio_region[0x0014].ExpectRead(0x00000000).ExpectWrite(0x01000000);  // Power down DP.

  audio_region[0x0008].ExpectRead(0xffffffff).ExpectWrite(0xfffffffd);  // Reset.
  audio_region[0x000c].ExpectRead(0x00000000).ExpectWrite(0x000cdc87);  // Fractional.
  audio_region[0x0008].ExpectRead(0x00000000).ExpectWrite(0x00003704);  // dn 55 dm 1.
  audio_region[0x0014].ExpectRead(0x00000000).ExpectWrite(0x0e000000);  // dp 7.
  audio_region[0x0008].ExpectRead(0x00000000).ExpectWrite(0x00000002);  // Not reset.

  audio_region[0x0014].ExpectRead(0xffffffff).ExpectWrite(0xfeffffff);  // Power up DP.
  audio_region[0x0018].ExpectRead(0xffffffff).ExpectWrite(0xfffffffe);  // Remove bypass.
  audio_region[0x0044].ExpectRead(0x00000000).ExpectWrite(0x00000004);  // Clock enable.

  EXPECT_OK(test.ClockImplSetRate(0, 196'608'000));

  audio_region.VerifyAll();
}

TEST(ClkSynTest, AvpllSetRateFractionalFor44100Hz) {
  ddk_mock::MockMmioRegRegion global_region(4, as370::kGlobalSize / 4);
  ddk_mock::MockMmioRegRegion audio_region(4, as370::kAudioGlobalSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(global_region, audio_region, unused);

  audio_region[0x0044].ExpectRead(0xffffffff).ExpectWrite(0xfffffffb);  // Clock disable.
  audio_region[0x0018].ExpectRead(0x00000000).ExpectWrite(0x00000001);  // Bypass.
  audio_region[0x0014].ExpectRead(0x00000000).ExpectWrite(0x01000000);  // Power down DP.

  audio_region[0x0008].ExpectRead(0xffffffff).ExpectWrite(0xfffffffd);  // Reset.
  audio_region[0x000c].ExpectRead(0x00000000).ExpectWrite(0x0093d102);  // Fractional.
  audio_region[0x0008].ExpectRead(0x00000000).ExpectWrite(0x00003204);  // dn 50 dm 1.
  audio_region[0x0014].ExpectRead(0x00000000).ExpectWrite(0x0e000000);  // dp 7.
  audio_region[0x0008].ExpectRead(0x00000000).ExpectWrite(0x00000002);  // Not reset.

  audio_region[0x0014].ExpectRead(0xffffffff).ExpectWrite(0xfeffffff);  // Power up DP.
  audio_region[0x0018].ExpectRead(0xffffffff).ExpectWrite(0xfffffffe);  // Remove bypass.
  audio_region[0x0044].ExpectRead(0x00000000).ExpectWrite(0x00000004);  // Clock enable.

  EXPECT_OK(test.ClockImplSetRate(0, 180'633'600));

  audio_region.VerifyAll();
}

TEST(ClkSynTest, AvpllSetRatePll1) {
  ddk_mock::MockMmioRegRegion global_region(4, as370::kGlobalSize / 4);
  ddk_mock::MockMmioRegRegion audio_region(4, as370::kAudioGlobalSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(global_region, audio_region, unused);

  audio_region[0x0044].ExpectRead(0xffffffff).ExpectWrite(0xfffffff7);  // Clock disable.
  audio_region[0x0038].ExpectRead(0x00000000).ExpectWrite(0x00000001);  // Bypass.
  audio_region[0x0034].ExpectRead(0x00000000).ExpectWrite(0x01000000);  // Power down DP.

  audio_region[0x0028].ExpectRead(0x00000000).ExpectWrite(0x0000e004);  // dn 224 dm 1.
  audio_region[0x0034].ExpectRead(0x00000000).ExpectWrite(0x0e000000);  // dp 7.

  audio_region[0x0034].ExpectRead(0xffffffff).ExpectWrite(0xfeffffff);  // Power up DP.
  audio_region[0x0038].ExpectRead(0xffffffff).ExpectWrite(0xfffffffe);  // Remove bypass.
  audio_region[0x0044].ExpectRead(0x00000000).ExpectWrite(0x00000008);  // Clock enable.

  EXPECT_OK(test.ClockImplSetRate(1, 800'000'000));

  audio_region.VerifyAll();
}

TEST(ClkSynTest, CpuPllSetRateBad) {
  ddk_mock::MockMmioRegRegion global_region(4, as370::kGlobalSize / 4);
  ddk_mock::MockMmioRegRegion audio_region(4, as370::kAudioGlobalSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(global_region, audio_region, unused);

  EXPECT_NOT_OK(test.ClockImplSetRate(2, 1'800'000'001));  // Too high.
  EXPECT_NOT_OK(test.ClockImplSetRate(2, 99'999'999));     // Too low.
}

TEST(ClkSynTest, CpuPllSetRate1800MHz) {
  ddk_mock::MockMmioRegRegion cpu_region(4, as370::kCpuSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(unused, unused, cpu_region);

  cpu_region[0x2000].ExpectWrite(0x00404806);
  cpu_region[0x2004].ExpectWrite(0x00000000);
  cpu_region[0x200c].ExpectWrite(0x22000000);

  EXPECT_OK(test.ClockImplSetRate(2, 1'800'000'000));

  cpu_region.VerifyAll();
}

TEST(ClkSynTest, CpuPllSetRate1000MHz) {
  ddk_mock::MockMmioRegRegion cpu_region(4, as370::kCpuSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(unused, unused, cpu_region);

  cpu_region[0x2000].ExpectWrite(0x00402806);
  cpu_region[0x2004].ExpectWrite(0x00000000);
  cpu_region[0x200c].ExpectWrite(0x22000000);

  EXPECT_OK(test.ClockImplSetRate(2, 1'000'000'000));

  cpu_region.VerifyAll();
}

TEST(ClkSynTest, CpuPllSetRate400MHz) {
  ddk_mock::MockMmioRegRegion cpu_region(4, as370::kCpuSize / 4);
  ddk_mock::MockMmioRegRegion unused(sizeof(uint32_t), as370::kCpuSize / 4);
  As370ClkTest test(unused, unused, cpu_region);

  cpu_region[0x2000].ExpectWrite(0x00403006);
  cpu_region[0x2004].ExpectWrite(0x00000000);
  cpu_region[0x200c].ExpectWrite(0x26000000);

  EXPECT_OK(test.ClockImplSetRate(2, 400'000'000));

  cpu_region.VerifyAll();
}

}  // namespace clk
