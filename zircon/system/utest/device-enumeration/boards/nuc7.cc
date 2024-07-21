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

TEST_F(DeviceEnumerationTest, Nuc7i5DNBTest) {
  if (!CheckTestMatch(NucType::NUC7i5DNB)) {
    return;
  }

  static const char* kNodeMonikers[] = {
      "dev.sys.platform.pt.PCI0.bus.00_02.0.00_02_0.intel-gpu-core",
      "dev.sys.platform.pt.PCI0.bus.00_02.0.00_02_0.intel-display-controller.display-coordinator",
      "dev.sys.platform.pt.PCI0.bus.00_14.0.00_14_0.xhci.usb-bus",
      "dev.sys.platform.pt.PCI0.bus.00_17.0.00_17_0.ahci",
      // TODO(https://fxbug.dev.42164781): Temporarily removed.
      // "dev.sys.platform.pt.PCI0.bus.00_1f.3.00_1f_3.intel-hda-000",
      // "dev.sys.platform.pt.PCI0.bus.00_1f.3.00_1f_3.intel-hda-controller",
      "dev.sys.platform.pt.PCI0.bus.00_1f.6.00_1f_6.e1000",
#ifdef include_packaged_drivers
      "dev.sys.platform.pt.PCI0.bus.01_00.0.01_00_0.iwlwifi-wlanphyimpl",
#endif
  };
  VerifyNodes(kNodeMonikers);

  // TODO(https://fxbug.dev/42075152): Fix the metadata problems in i2c.cm and re-enable this.
  // static const char* kDfv1DevicePaths[] = {
  //     "dev.sys.platform.pt.PCI0.bus.00_15.0.00_15_0.i2c-bus-9d60",
  //     "dev.sys.platform.pt.PCI0.bus.00_15.1.00_15_1.i2c-bus-9d61",
  // };
  // ASSERT_NO_FATAL_FAILURE(TestRunner(kDfv1DevicePaths, std::size(kDfv1DevicePaths)));
}

}  // namespace
