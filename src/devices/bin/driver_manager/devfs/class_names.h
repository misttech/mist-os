// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_CLASS_NAMES_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_CLASS_NAMES_H_

namespace driver_manager {

// This is the list of all the class names that were possible from when devfs used protodefs.h
// to track class names.  This list now represents class names that automatically have a folder
// created under /dev/class when devfs first starts up.
//  Other class names are allowed, but will not have a folder created for them
// until they advertise through devfs.
const char* kEagerClassNames[] = {"acpi",
                                  "acpi-device",
                                  "adb",
                                  "adc",
                                  "aml-dsp",
                                  "aml-mailbox",
                                  "aml-ram",
                                  "at-transport",
                                  "audio",
                                  "audio-composite",
                                  "audio-input",
                                  "audio-output",
                                  "backlight",
                                  "battery",
                                  "block",
                                  "block-partition",
                                  "block-volume",
                                  "bt-emulator",
                                  "bt-gatt-svc",
                                  "bt-hci",
                                  "bt-host",
                                  "bt-transport",
                                  "bt-vendor",
                                  "camera",
                                  "chromeos-acpi",
                                  "clock-impl",
                                  "codec",
                                  "console",
                                  "cpu-ctrl",
                                  "ctap",
                                  "dai",
                                  "device",
                                  "display-coordinator",
                                  "dsi-base",
                                  "ethernet",
                                  "ethernet-impl",
                                  "fan",
                                  "fastboot",
                                  "framebuffer",
                                  "goldfish-address-space",
                                  "goldfish-control",
                                  "goldfish-pipe",
                                  "goldfish-sync",
                                  "gpio",
                                  "gpu",
                                  "gpu-dependency-injection",
                                  "gpu-performance-counters",
                                  "gpu-thermal",
                                  "hidbus",
                                  "hrtimer",
                                  "i2c",
                                  "i2c-hid",
                                  "i2c-impl",
                                  "input",
                                  "input-report",
                                  "input-report-inject",
                                  "intel-gpu-core",
                                  "intel-hda",
                                  "intel-hda-codec",
                                  "intel-hda-dsp",
                                  "iommu",
                                  "isp",
                                  "light",
                                  "mali-util",
                                  "media-codec",
                                  "midi",
                                  "mlg",
                                  "nand",
                                  "network",
                                  "ot-radio",
                                  "overnet-usb",
                                  "pci",
                                  "platform-dev",
                                  "power",
                                  "power-sensor",
                                  "pty",
                                  "pwm-impl",
                                  "qmi-transport",
                                  "radar",
                                  "rawnand",
                                  "registers",
                                  "rtc",
                                  "sdhci",
                                  "sdio",
                                  "sdmmc",
                                  "securemem",
                                  "serial",
                                  "serial-impl",
                                  "serial-impl-async",
                                  "skip-block",
                                  "spi",
                                  "spi-impl",
                                  "suspend",
                                  "sysmem",
                                  "tdh",
                                  "tee",
                                  "temperature",
                                  "test",
                                  "test-asix-function",
                                  "test-compat-child",
                                  "test-power-child",
                                  "thermal",
                                  "tpm",
                                  "tpm-impl",
                                  "trippoint",
                                  "usb-cache-test",
                                  "usb-dbc",
                                  "usb-dci",
                                  "usb-device",
                                  "usb-function",
                                  "usb-hci",
                                  "usb-hci-test",
                                  "usb-peripheral",
                                  "usb-phy",
                                  "usb-tester",
                                  "virtual-bus-test",
                                  "virtual-camera-factory",
                                  "vsock",
                                  "wlan-factory",
                                  "wlan-fullmac",
                                  "wlan-fullmac-impl",
                                  "wlanphy",
                                  "wlanphy-impl",
                                  "wlan-softmac",
                                  "zxcrypt"};

// TODO(https://fxbug.dev/42064970): shrink this list to zero.
//
// Do not add to this list.
//
// These classes have clients that rely on the numbering scheme starting at
// 000 and increasing sequentially. This list was generated using:
//
// rg -IoN --no-ignore -g '!out/' -g '!*.md' '\bclass/[^/]+/[0-9]{3}\b' | \
// sed -E 's|class/(.*)/[0-9]{3}|"\1",|g' | sort | uniq
// The uint8_t that the class name maps to tracks the next available device number.
std::unordered_map<std::string_view, uint8_t> classes_that_assume_ordering({
    // TODO(https://fxbug.dev/42065012): Remove.
    {"adc", 0},

    // TODO(https://fxbug.dev/42065013): Remove.
    {"aml-ram", 0},

    // TODO(https://fxbug.dev/42065014): Remove.
    // TODO(https://fxbug.dev/42065080): Remove.
    {"backlight", 0},

    // TODO(https://fxbug.dev/42068339): Remove.
    {"block", 0},

    // TODO(https://fxbug.dev/42065067): Remove.
    {"goldfish-address-space", 0},
    {"goldfish-control", 0},
    {"goldfish-pipe", 0},

    // TODO(https://fxbug.dev/42065072): Remove.
    {"ot-radio", 0},

    // TODO(https://fxbug.dev/42065076): Remove.
    {"securemem", 0},

    // TODO(https://fxbug.dev/42065009): Remove.
    // TODO(https://fxbug.dev/42065080): Remove.
    {"temperature", 0},

    // TODO(https://fxbug.dev/42065080): Remove.
    {"thermal", 0},
});

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_CLASS_NAMES_H_
