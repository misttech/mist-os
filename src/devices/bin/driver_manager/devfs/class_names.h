// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_CLASS_NAMES_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_CLASS_NAMES_H_

#include <string>
#include <unordered_map>

namespace driver_manager {

// Specifies the service and member protocol that will map to a class name
struct ServiceEntry {
  enum AdvertiseState {
    NO_ADVERTISEMENT,
    DEVFS_ONLY,
    SERVICE_ONLY,
    DEVFS_AND_SERVICE,
  };

  // Indicates for a given class name whether the service should be advertised,
  // and whether a devfs entry should be advertised.
  AdvertiseState state;
  // The name of the service that should be advertised for a class name.
  // The format is: "the.fidl.namespace.ServiceName"
  std::string service_name;
  // The name of the member of the service that corresponds to the protocol
  // that is normally advertised through dev/class/class_name
  std::string member_name;
};

// The key values in this map represent class names that devfs recognizes.
// Each class name has a folder automatically created under /dev/class
// when devfs first starts up.
// The ServiceEntry that corresponds to each class name specifies how devfs should
// map the offered protocol to the member protocol of a service.
// As an example,
// For a fidl protocol and service defined as:
//   library fidl.examples.echo;
//   protocol DriverEcho {...}
//   service DriverEchoService {
//       echo_device client_end:DriverEcho;
//   };
//  imagine that /dev/class/driver_test gave access to a fidl.examples.echo::DriverEcho
// protocol.  To automatically advertise that protocol as a service, you would
// update the driver_test entry in kClassNameToService to:
// {"driver_test", {ServiceEntry::DEVFS_AND_SERVICE,
//                        "fidl.examples.echo.DriverEchoService", "echo_device"}},
const std::unordered_map<std::string_view, ServiceEntry> kClassNameToService = {
    {"acpi", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"adb", {ServiceEntry::DEVFS_ONLY, "", ""}},  // Proto: fuchsia.hardware.adb.Device
    {"adc", {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.adc.Service", "device"}},
    {"aml-ram", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"audio-composite",
     {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.audio.CompositeConnectorService",
      "composite_connector"}},
    {"audio-input",
     {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.audio.StreamConfigConnectorService",
      "stream_config_connector"}},
    {"audio-output",
     {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.audio.StreamConfigConnectorService",
      "stream_config_connector"}},
    {"audio", {ServiceEntry::DEVFS_ONLY, "", ""}},  // Protocol: "fuchsia.hardware.audio.Device"
    {"backlight",
     {ServiceEntry::DEVFS_ONLY, "", ""}},  // Proto: "fuchsia.hardware.backlight.Service"
    {"block-partition", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"block", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"block-volume", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"bt-hci",
     {ServiceEntry::DEVFS_ONLY, "", ""}},  // Protocol: "fuchsia.hardware.bluetooth.Vendor",
    {"camera", {ServiceEntry::DEVFS_ONLY, "", ""}},  // Protocol: " fuchsia.hardware.camera.Device",
    {"clock-impl",
     {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.clock.measure.Service", "measurer"}},
    {"codec",
     {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.audio.CodecConnectorService",
      "codec_connector"}},
    {"console", {ServiceEntry::DEVFS_ONLY, "", ""}},  // Protocol: "fuchsia.hardware.pty.Device",
    {"cpu-ctrl", {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.cpu.ctrl.Service", "device"}},
    {"dai",
     {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.audio.DaiConnectorService",
      "dai_connector"}},
    {"device", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"devfs_service_test",
     {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.services.test.Device", "control"}},
    {"display-coordinator",
     {ServiceEntry::DEVFS_ONLY, "", ""}},  // Protocol:  "fuchsia.hardware.display.Provider",
    {"fan", {ServiceEntry::DEVFS_AND_SERVICE, "", ""}},  // Protocol: "fuchsia.hardware.fan.Device",
    {"fastboot",
     {ServiceEntry::DEVFS_ONLY, "", ""}},  // Proto: fuchsia.hardware.fastboot.FastbootImpl
    {"goldfish-address-space", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"goldfish-control",
     {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.goldfish.AddressSpaceService", "device"}},
    {"goldfish-pipe",
     {ServiceEntry::DEVFS_ONLY, "", ""}},  // Protocol:  "fuchsia.hardware.goldfish.PipeDevice",
    {"goldfish-sync", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"gpu-dependency-injection", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"gpu-performance-counters", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"gpu", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"hrtimer", {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.hrtimer.Service", "device"}},
    {"i2c", {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.i2c.Service", "device"}},
    {"input-report", {ServiceEntry::DEVFS_ONLY, "", ""}},  // Proto: "fuchsia.input.report.Device",
    {"input", {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.input.Service", "controller"}},
    {"light", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"mali-util",
     {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.gpu.mali.Service", "arm_mali"}},
    {"media-codec", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"midi", {ServiceEntry::DEVFS_ONLY, "", ""}},  // Protocol:  "fuchsia.hardware.midi.Controller",
    {"nand", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"network",
     {ServiceEntry::DEVFS_ONLY, "", ""}},  // Protocol: "fuchsia.hardware.network.DeviceInstance",
    {"ot-radio", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"overnet-usb", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"power-sensor", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"power", {ServiceEntry::DEVFS_ONLY, "", ""}},  // Proto: "fuchsia.hardware.powersource.Source",
    {"radar",
     {ServiceEntry::DEVFS_ONLY, "", ""}},  // Proto: fuchsia.hardware.radar.RadarBurstReader
    {"registers",
     {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.registers.Service", "device"}},
    {"rtc", {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.rtc.Service", "device"}},
    {"sdio", {ServiceEntry::DEVFS_AND_SERVICE, "fuchsia.hardware.sdio.DriverService", "device"}},
    {"securemem", {ServiceEntry::DEVFS_ONLY, "", ""}},  // Proto: fuchsia.hardware.securemem.Device
    {"serial", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"skip-block",
     {ServiceEntry::DEVFS_ONLY, "", ""}},  // Protocol: "fuchsia.hardware.skipblock.SkipBlock",
    {"spi", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"sysmem", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"tee", {ServiceEntry::DEVFS_ONLY, "fuchsia.hardware.tee.Service", "device_connector"}},
    {"temperature",
     {ServiceEntry::DEVFS_ONLY, "", ""}},  // Proto: fuchsia.hardware.temperature.Device
    {"test", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"thermal", {ServiceEntry::DEVFS_ONLY, "", ""}},  // Proto:  "fuchsia.hardware.thermal.Device",
    {"tpm", {ServiceEntry::DEVFS_ONLY, "", ""}},      // Proto: "fuchsia.tpm.TpmDevice",
    {"trippoint", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"usb-device", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"usb-peripheral",
     {ServiceEntry::DEVFS_ONLY, "", ""}},  // Proto: "fuchsia.hardware.usb.peripheral.Device",
    {"usb-tester", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"virtual-bus-test", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"vsock", {ServiceEntry::DEVFS_ONLY, "", ""}},
    {"wlanphy", {ServiceEntry::DEVFS_ONLY, "", ""}},
};

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
