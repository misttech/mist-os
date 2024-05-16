// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

enum NucType {
  NUC7i5DNB,
  NUC11TNBv5,
};

std::string NucTypeToString(NucType nuc_type) {
  if (nuc_type == NucType::NUC7i5DNB) {
    return "NUC7i5DNB";
  } else if (nuc_type == NucType::NUC11TNBv5) {
    return "NUC11TNBv5";
  }

  return "N/A";
}

std::variant<NucType, std::string> GetNucType() {
  zx::result sys_info = component::Connect<fuchsia_sysinfo::SysInfo>();
  EXPECT_EQ(ZX_OK, sys_info.status_value(), "Couldn't connect to SysInfo.");

  const fidl::WireResult result = fidl::WireCall(sys_info.value())->GetBoardName();
  EXPECT_EQ(ZX_OK, result.status(), "Couldn't call GetBoardName.");

  const fidl::WireResponse response = result.value();
  EXPECT_EQ(ZX_OK, response.status, "GetBoardName failed.");

  const std::string_view board_name = response.name.get();
  if (board_name == "NUC7i5DNB") {
    return NucType::NUC7i5DNB;
  } else if (board_name == "NUC11TNBv5") {
    return NucType::NUC11TNBv5;
  }

  return std::string(board_name);
}

bool CheckTestMatch(NucType desired_nuc_type) {
  std::variant nuc_type = GetNucType();
  const std::string* unknown_nuc_type = std::get_if<std::string>(&nuc_type);
  if (unknown_nuc_type) {
    printf("Skipping unknown NUC type: %s", unknown_nuc_type->c_str());
    return false;
  }

  NucType nuc = std::get<NucType>(nuc_type);
  if (nuc != desired_nuc_type) {
    printf("Skipping NUC type: %s", NucTypeToString(nuc).c_str());
    return false;
  }

  return true;
}

TEST_F(DeviceEnumerationTest, Nuc11TNBv5Test) {
  if (!CheckTestMatch(NucType::NUC11TNBv5)) {
    return;
  }

  static const char* kDevicePaths[] = {
      "sys/platform/pt/PC00/bus/00:02.0/00_02_0/intel_i915/intel-gpu-core",
      "sys/platform/pt/PC00/bus/00:02.0/00_02_0/intel_i915/intel-display-controller/display-coordinator",
      "sys/platform/pt/PC00/bus/00:14.0/00_14_0/xhci/usb-bus",
      "sys/platform/pt/PC00/bus/00:17.0/00_17_0/ahci",
  };
  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));

// TODO(b/42178510): Remove older path once we migrate iwlwifi to composite node specs.
#ifdef include_packaged_drivers
  static const char* kIwlwifiDevicePaths[] = {
      "sys/platform/pt/PC00/bus/00:14.3/00:14.3/iwlwifi-wlanphyimpl",
      "sys/platform/pt/PC00/bus/00:14.3/00_14_3/iwlwifi-wlanphyimpl",
  };
  ASSERT_NO_FATAL_FAILURE(device_enumeration::WaitForOne(
      cpp20::span(kIwlwifiDevicePaths, std::size(kIwlwifiDevicePaths))));
#endif
}

}  // namespace
