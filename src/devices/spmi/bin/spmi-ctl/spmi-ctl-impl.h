// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <fidl/fuchsia.hardware.spmi/cpp/fidl.h>

class SpmiCtl {
 public:
  explicit SpmiCtl(fidl::ClientEnd<fuchsia_hardware_spmi::Device> device) {
    test_client_.emplace(std::move(device));  // Allows injecting the device from tests.
  }
  SpmiCtl() = default;

  int Execute(int argc, char** argv);

 private:
  fidl::SyncClient<fuchsia_hardware_spmi::Device> GetSpmiClient(std::string target,
                                                                std::string sub_target);
  void ListDevices();

  std::optional<fidl::SyncClient<fuchsia_hardware_spmi::Device>> test_client_;
};
