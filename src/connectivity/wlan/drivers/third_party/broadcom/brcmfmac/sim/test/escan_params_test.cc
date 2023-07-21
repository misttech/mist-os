// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include <fuchsia/hardware/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <zircon/errors.h>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"

namespace wlan::brcmfmac {
namespace {

constexpr zx::duration kSimulatedClockDuration = zx::sec(10);

}  // namespace

constexpr uint64_t kScanTxnId = 0x4a65616e6e65;
const uint8_t kDefaultChannelsList[11] = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11};

// For this test, we don't want to use the default scan handlers provided by SimInterface
class EscanArgsIfc : public SimInterface {
 public:
  void OnScanEnd(OnScanEndRequestView request, fdf::Arena& arena,
                 OnScanEndCompleter::Sync& completer) override;
  bool ScanCompleted() { return scan_completed_; }
  wlan_fullmac::WlanScanResult ScanResult() { return scan_result_; }

 private:
  bool scan_completed_ = false;
  wlan_fullmac::WlanScanResult scan_result_;
};

void EscanArgsIfc::OnScanEnd(OnScanEndRequestView request, fdf::Arena& arena,
                             OnScanEndCompleter::Sync& completer) {
  EXPECT_EQ(request->end.txn_id, kScanTxnId);
  scan_completed_ = true;
  scan_result_ = request->end.code;
  completer.buffer(arena).Reply();
}

class EscanArgsTest : public SimTest {
 public:
  void Init();
  void RunScanTest(const wlan_fullmac::WlanFullmacImplStartScanRequest& req);

 protected:
  EscanArgsIfc client_ifc_;
};

void EscanArgsTest::Init() {
  ASSERT_EQ(SimTest::Init(), ZX_OK);
  ASSERT_EQ(StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc_), ZX_OK);
}

void EscanArgsTest::RunScanTest(const wlan_fullmac::WlanFullmacImplStartScanRequest& req) {
  auto result = client_ifc_.client_.buffer(client_ifc_.test_arena_)->StartScan(req);
  ASSERT_TRUE(result.ok());
  env_->Run(kSimulatedClockDuration);
  ASSERT_TRUE(client_ifc_.ScanCompleted());
}

// Verify that invalid scan params result in a failed scan result
TEST_F(EscanArgsTest, BadScanArgs) {
  Init();
  {
    auto builder = wlan_fullmac::WlanFullmacImplStartScanRequest::Builder(client_ifc_.test_arena_);

    builder.txn_id(kScanTxnId);
    builder.scan_type(wlan_fullmac::WlanScanType::kActive);
    builder.channels(
        fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(kDefaultChannelsList), 11));
    builder.min_channel_time(0);
    builder.max_channel_time(0);

    // Dwell time of zero
    RunScanTest(builder.Build());
  }
  EXPECT_NE(client_ifc_.ScanResult(), wlan_fullmac::WlanScanResult::kSuccess);

  // min dwell time > max dwell time
  {
    auto builder = wlan_fullmac::WlanFullmacImplStartScanRequest::Builder(client_ifc_.test_arena_);

    builder.txn_id(kScanTxnId);
    builder.scan_type(wlan_fullmac::WlanScanType::kActive);
    builder.channels(
        fidl::VectorView<uint8_t>::FromExternal(const_cast<uint8_t*>(kDefaultChannelsList), 11));
    builder.min_channel_time(SimInterface::kDefaultActiveScanDwellTimeMs + 1);
    builder.max_channel_time(SimInterface::kDefaultActiveScanDwellTimeMs);

    // Dwell time of zero
    RunScanTest(builder.Build());
  }
  EXPECT_NE(client_ifc_.ScanResult(), wlan_fullmac::WlanScanResult::kSuccess);
}

TEST_F(EscanArgsTest, EmptyChannelList) {
  Init();
  auto builder = wlan_fullmac::WlanFullmacImplStartScanRequest::Builder(client_ifc_.test_arena_);

  builder.txn_id(kScanTxnId), builder.scan_type(wlan_fullmac::WlanScanType::kActive),
      builder.min_channel_time(SimInterface::kDefaultActiveScanDwellTimeMs + 1);
  builder.max_channel_time(SimInterface::kDefaultActiveScanDwellTimeMs);

  RunScanTest(builder.Build());
  EXPECT_EQ(client_ifc_.ScanResult(), wlan_fullmac::WlanScanResult::kInvalidArgs);
}

}  // namespace wlan::brcmfmac
