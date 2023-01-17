// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.ftdi/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/ddk/platform-defs.h>

#include <filesystem>

void PrintHelp() {
  printf(
      "Usage: create_ssd1306 \n \
      create_ssd1306: This program creates an I2C bus on the FTDI 232H breakout chip \n \
         and programs it to have the ssd1306 display brought up as an I2C device. If this \n \
         completes successfully, `dm dump` should have the 'ftdi-i2c' device and the \n \
         'ssd1306' device. The ssd1306 device should appear under /dev/class/dotmatrix-display \n \
\n \
         PLEASE NOTE: The I2C bus on the 232H must be used as follows: \n \
            Pin 0 - SCL \n \
            Pins 1 & 2 - SDA and must be wired together\n");
}

int main(int argc, char** argv) {
  if (argc > 1) {
    PrintHelp();
    return 0;
  }

  constexpr char path[] = "/dev/class/serial-impl/";
  for (const auto& entry : std::filesystem::directory_iterator{path}) {
    zx::result device = component::Connect<fuchsia_hardware_ftdi::Device>(entry.path().c_str());
    if (device.is_error()) {
      printf("connecting to %s failed: %s\n", entry.path().c_str(), device.status_string());
      return 1;
    }
    fuchsia_hardware_ftdi::wire::I2cBusLayout layout = {
        .scl = 0,
        .sda_out = 1,
        .sda_in = 2,
    };
    fuchsia_hardware_ftdi::wire::I2cDevice i2c_dev = {
        // This is the I2C address for the SSD1306.
        .address = 0x3c,
        // These are the SSD1306 driver binding rules.
        .vid = PDEV_VID_GENERIC,
        .pid = PDEV_PID_GENERIC,
        .did = PDEV_DID_SSD1306,
    };

    const fidl::Status status = fidl::WireCall(device.value())->CreateI2C(layout, i2c_dev);
    if (!status.ok()) {
      printf("CreateI2C on %s failed: %s\n", entry.path().c_str(),
             status.FormatDescription().c_str());
      return 1;
    }
    return 0;
  }

  printf("%s contained no entries\n", path);
  return 1;
}
