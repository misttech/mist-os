// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_FIRMWARE_LOADER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_FIRMWARE_LOADER_H_

#include <string>

#include "lib/async/dispatcher.h"
#include "src/devices/bin/driver_manager/v1/driver.h"

struct LoadFirmwareResult {
  zx::vmo vmo;
  size_t size;
};

class FirmwareLoader {
 public:
  FirmwareLoader(async_dispatcher_t* firmware_dispatcher, std::string path_prefix);

  void LoadFirmware(const Driver* driver, const char* path,
                    fit::callback<void(zx::result<LoadFirmwareResult>)> cb) const;

 private:
  async_dispatcher_t* const firmware_dispatcher_;
  std::string path_prefix_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_FIRMWARE_LOADER_H_
