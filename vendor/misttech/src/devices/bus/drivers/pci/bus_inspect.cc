// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/inspect/cpp/vmo/types.h>
#include <zircon/syscalls/object.h>

#include <algorithm>

#include "src/devices/bus/drivers/pci/bus.h"

namespace pci {

class HexHelper {
 public:
  explicit HexHelper(uint64_t value) { snprintf(value_.data(), value_.size(), "0x%lx", value); }
  const char* get() { return value_.data(); }

 private:
  // 16 digits for u64, 2 for 0x, 1 for a \0.
  std::array<char, 19> value_;
};

void Bus::InspectInit() {
  bus_node_ = inspector_.GetRoot().CreateChild(BusInspect::kBus);
  devices_node_ = inspector_.GetRoot().CreateChild(BusInspect::kDevices);
}

// Record everything passed to the PCI Bus driver via the PcirootProtocol's |GetPciPlatformInfo|.
void Bus::InspectRecordPlatformInformation() {
  bus_node_.RecordString(BusInspect::kName, info_.name);
  bus_node_.RecordString(BusInspect::kBusStart, HexHelper(info_.start_bus_num).get());
  bus_node_.RecordString(BusInspect::kBusEnd, HexHelper(info_.end_bus_num).get());
  bus_node_.RecordString(BusInspect::kSegmentGroup, HexHelper(info_.segment_group).get());

  if (ecam_) {
    bus_node_.RecordString(BusInspect::kEcam,
                           HexHelper(reinterpret_cast<uint64_t>(ecam_->get())).get());
  }

  if (!acpi_devices_.empty()) {
    acpi_node_ = bus_node_.CreateStringArray(BusInspect::kAcpiDevices, acpi_devices_.size());

    for (size_t i = 0; i < acpi_devices_.size(); i++) {
      char bdf[10];
      snprintf(bdf, std::size(bdf), "%02x:%02x.%1x", acpi_devices_[i].bus_id,
               acpi_devices_[i].device_id, acpi_devices_[i].function_id);
      acpi_node_.Set(i, bdf);
    }
  }

  if (!irqs_.empty()) {
    auto vectors_node = bus_node_.CreateStringArray(BusInspect::kVectors, irqs_.size());
    for (size_t i = 0; i < irqs_.size(); i++) {
      vectors_node.Set(i, HexHelper(irqs_[i].vector).get());
    }
    bus_node_.Record(std::move(vectors_node));
  }

  // These are used dozens of times each so we can save memory using StringReferences.
  if (!irq_routing_entries_.empty()) {
    auto routing_node = bus_node_.CreateChild(BusInspect::kIrqRoutingEntries);
    for (auto& entry : irq_routing_entries_) {
      auto node = routing_node.CreateChild(bus_node_.UniqueName(""));
      node.RecordString(BusInspect::kPortDeviceId, HexHelper(entry.port_device_id).get());
      node.RecordString(BusInspect::kPortFunctionId, HexHelper(entry.port_function_id).get());
      node.RecordString(BusInspect::kDeviceId, HexHelper(entry.device_id).get());
      auto pins = node.CreateStringArray(kPins, std::size(entry.pins));
      for (size_t i = 0; i < std::size(entry.pins); i++) {
        pins.Set(i, HexHelper(entry.pins[i]).get());
      }
      node.Record(std::move(pins));
      bus_node_.Record(std::move(node));
    }
    bus_node_.Record(std::move(routing_node));
  }
}

}  // namespace pci
