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
  using AdvertiseState = uint8_t;
  static constexpr AdvertiseState kNone = 0;
  static constexpr AdvertiseState kDevfs = 1;
  static constexpr AdvertiseState kService = 2;
  static constexpr AdvertiseState kDevfsAndService = kDevfs | kService;
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
// {"driver_test", {ServiceEntry::kDevfsAndService,
//                        "fidl.examples.echo.DriverEchoService", "echo_device"}},
const std::unordered_map<std::string_view, ServiceEntry> kClassNameToService = {
    {"acpi", {ServiceEntry::kDevfs, "", ""}},
    {"adb", {ServiceEntry::kDevfs, "fuchsia.hardware.adb.Service", "adb"}},
    {"adc", {ServiceEntry::kDevfsAndService, "fuchsia.hardware.adc.Service", "device"}},
    {"aml-ram", {ServiceEntry::kDevfs, "fuchsia.hardware.ram.metrics.Service", "device"}},
    {"audio-composite",
     {ServiceEntry::kDevfsAndService, "fuchsia.hardware.audio.CompositeConnectorService",
      "composite_connector"}},
    {"audio-input",
     {ServiceEntry::kDevfsAndService, "fuchsia.hardware.audio.StreamConfigConnectorService",
      "stream_config_connector"}},
    {"audio-output",
     {ServiceEntry::kDevfsAndService, "fuchsia.hardware.audio.StreamConfigConnectorService",
      "stream_config_connector"}},
    {"audio", {ServiceEntry::kDevfs, "", ""}},  // Protocol: "fuchsia.hardware.audio.Device"
    {"backlight", {ServiceEntry::kDevfs, "fuchsia.hardware.backlight.Service", "backlight"}},
    {"block-partition", {ServiceEntry::kDevfs, "", ""}},
    {"block", {ServiceEntry::kDevfs, "", ""}},
    {"block-volume", {ServiceEntry::kDevfs, "", ""}},
    {"bt-emulator", {ServiceEntry::kDevfs, "fuchsia.hardware.bluetooth.EmulatorService", "device"}},
    {"bt-hci", {ServiceEntry::kDevfs, "fuchsia.hardware.bluetooth.Service", "vendor"}},
    {"camera", {ServiceEntry::kDevfs, "fuchsia.hardware.camera.Service", "device"}},
    {"clock-impl",
     {ServiceEntry::kDevfsAndService, "fuchsia.hardware.clock.measure.Service", "measurer"}},
    {"codec",
     {ServiceEntry::kDevfsAndService, "fuchsia.hardware.audio.CodecConnectorService",
      "codec_connector"}},
    {"console", {ServiceEntry::kDevfs, "fuchsia.hardware.pty.Service", "device"}},
    {"cpu-ctrl", {ServiceEntry::kDevfsAndService, "fuchsia.hardware.cpu.ctrl.Service", "device"}},
    {"dai",
     {ServiceEntry::kDevfsAndService, "fuchsia.hardware.audio.DaiConnectorService",
      "dai_connector"}},
    {"device", {ServiceEntry::kDevfs, "", ""}},
    {"devfs_service_test",
     {ServiceEntry::kDevfsAndService, "fuchsia.services.test.Device", "control"}},
    {"display-coordinator", {ServiceEntry::kDevfs, "fuchsia.hardware.display.service", "provider"}},
    {"fan", {ServiceEntry::kDevfsAndService, "fuchsia.hardware.fan.Service", "device"}},
    {"fastboot", {ServiceEntry::kDevfs, "fuchsia.hardware.fastboot.Service", "fastboot"}},
    {"goldfish-address-space",
     {ServiceEntry::kDevfs, "fuchsia.hardware.goldfish.AddressSpaceService", "device"}},
    {"goldfish-control",
     {ServiceEntry::kDevfsAndService, "fuchsia.hardware.goldfish.ControlService", "device"}},
    {"goldfish-pipe",
     {ServiceEntry::kDevfs, "fuchsia.hardware.goldfish.ControllerService", "device"}},
    {"goldfish-sync", {ServiceEntry::kDevfs, "fuchsia.hardware.goldfish.SyncService", "device"}},
    {"gpio", {ServiceEntry::kDevfsAndService, "fuchsia.hardware.pin.DebugService", "device"}},
    {"gpu-dependency-injection",
     {ServiceEntry::kDevfs, "fuchsia.gpu.magma.DependencyInjectionService", "device"}},
    {"gpu-performance-counters",
     {ServiceEntry::kDevfs, "fuchsia.gpu.magma.PerformanceCounterService", "access"}},
    {"gpu", {ServiceEntry::kDevfs, "fuchsia.gpu.magma.Service", "device"}},
    {"hrtimer", {ServiceEntry::kDevfsAndService, "fuchsia.hardware.hrtimer.Service", "device"}},
    {"i2c", {ServiceEntry::kDevfsAndService, "fuchsia.hardware.i2c.Service", "device"}},
    {"input-report", {ServiceEntry::kDevfs, "fuchsia.input.report.Service", "input_device"}},
    {"input", {ServiceEntry::kDevfsAndService, "fuchsia.hardware.input.Service", "controller"}},
    {"light", {ServiceEntry::kDevfs, "fuchsia.hardware.light.LightService", "light"}},
    {"mali-util",
     {ServiceEntry::kDevfsAndService, "fuchsia.hardware.gpu.mali.Service", "arm_mali"}},
    {"media-codec", {ServiceEntry::kDevfs, "fuchsia.hardware.mediacodec.Service", "device"}},
    {"midi", {ServiceEntry::kDevfs, "fuchsia.hardware.midi.Service", "controller"}},
    {"nand", {ServiceEntry::kDevfs, "", ""}},
    {"network", {ServiceEntry::kDevfs, "fuchsia.hardware.network.Service", "device"}},
    {"ot-radio", {ServiceEntry::kDevfs, "fuchsia.lowpan.spinel.Service", "device_setup"}},
    {"power-sensor", {ServiceEntry::kDevfs, "fuchsia.hardware.power.sensor.Service", "device"}},
    {"power", {ServiceEntry::kDevfs, "fuchsia.hardware.powersource.Service", "source"}},
    {"radar", {ServiceEntry::kDevfs, "fuchsia.hardware.radar.Service", "device"}},
    {"registers", {ServiceEntry::kDevfsAndService, "fuchsia.hardware.registers.Service", "device"}},
    {"rtc", {ServiceEntry::kDevfsAndService, "fuchsia.hardware.rtc.Service", "device"}},
    {"sdio", {ServiceEntry::kDevfsAndService, "fuchsia.hardware.sdio.DriverService", "device"}},
    {"serial", {ServiceEntry::kDevfs, "fuchsia.hardware.Serial.ProxyService", "device"}},
    {"skip-block", {ServiceEntry::kDevfs, "fuchsia.hardware.skipblock.Service", "skipblock"}},
    {"spi", {ServiceEntry::kDevfs, "fuchsia.hardware.spi.ControllerService", "device"}},
    {"tee", {ServiceEntry::kDevfs, "fuchsia.hardware.tee.Service", "device_connector"}},
    {"temperature", {ServiceEntry::kDevfs, "fuchsia.hardware.temperature.Service", "device"}},
    {"test", {ServiceEntry::kDevfs, "", ""}},
    {"test-asix-function", {ServiceEntry::kDevfs, "fuchsia.hardware.ax88179.Service", "hooks"}},
    {"thermal", {ServiceEntry::kDevfs, "fuchsia.hardware.thermal.Service", "device"}},
    {"tpm", {ServiceEntry::kDevfs, "fuchsia.tpm.Service", "device"}},
    {"trippoint",
     {ServiceEntry::kDevfs, "fuchsia.hardware.trippoint.TripPointService", "trippoint"}},
    {"usb-device", {ServiceEntry::kDevfs, "fuchsia.hardware.usb.device.Service", "device"}},
    {"usb-tester", {ServiceEntry::kDevfs, "fuchsia.hardware.usb.tester.Service", "device"}},
    {"virtual-bus-test",
     {ServiceEntry::kDevfs, "fuchsia.hardware.usb.virtualbustest.Service", "device"}},
    {"vsock", {ServiceEntry::kDevfs, "fuchsia.hardware.vsock.Service", "device"}},
    {"wlanphy", {ServiceEntry::kDevfs, "fuchsia.wlan.device.Service", "device"}},
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

    // TODO(https://fxbug.dev/42065009): Remove.
    // TODO(https://fxbug.dev/42065080): Remove.
    {"temperature", 0},

    // TODO(https://fxbug.dev/42065080): Remove.
    {"thermal", 0},
});

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_DEVFS_CLASS_NAMES_H_
