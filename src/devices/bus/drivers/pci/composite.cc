// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/pci/composite.h"

#include <lib/ddk/binding_driver.h>

#include <bind/fuchsia/acpi/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/pci/cpp/bind.h>
#include <bind/fuchsia/sysmem/cpp/bind.h>

namespace pci {

zx_status_t AddLegacyComposite(zx_device_t* pci_dev_parent, const char* composite_name,
                               const CompositeInfo& info) {
  const uint32_t kPciBindTopo = BIND_PCI_TOPO_PACK(info.bus_id, info.dev_id, info.func_id);

  // clang-format off
  const zx_device_prop_t kPciDeviceProps[] = {
      {BIND_FIDL_PROTOCOL, 0, bind_fuchsia_pci::BIND_FIDL_PROTOCOL_DEVICE},
      {BIND_PCI_VID, 0, info.vendor_id},
      {BIND_PCI_DID, 0, info.device_id},
      {BIND_PCI_CLASS, 0, info.class_id},
      {BIND_PCI_SUBCLASS, 0, info.subclass},
      {BIND_PCI_INTERFACE, 0, info.program_interface},
      {BIND_PCI_REVISION, 0, info.revision_id},
      {BIND_PCI_TOPO, 0, kPciBindTopo},
  };
  // clang-format on

  const zx_bind_inst_t kSysmemMatch[] = {
      BI_MATCH_IF(EQ, BIND_FIDL_PROTOCOL, ZX_FIDL_PROTOCOL_SYSMEM),
  };

  const device_fragment_part_t kSysmemFragment[] = {
      {std::size(kSysmemMatch), kSysmemMatch},
  };

  const zx_bind_inst_t kPciFragmentMatch[] = {
      BI_ABORT_IF(NE, BIND_FIDL_PROTOCOL, bind_fuchsia_pci::BIND_FIDL_PROTOCOL_DEVICE),
      BI_ABORT_IF(NE, BIND_PCI_VID, info.vendor_id),
      BI_ABORT_IF(NE, BIND_PCI_DID, info.device_id),
      BI_ABORT_IF(NE, BIND_PCI_CLASS, info.class_id),
      BI_ABORT_IF(NE, BIND_PCI_SUBCLASS, info.subclass),
      BI_ABORT_IF(NE, BIND_PCI_INTERFACE, info.program_interface),
      BI_ABORT_IF(NE, BIND_PCI_REVISION, info.revision_id),
      BI_ABORT_IF(EQ, BIND_COMPOSITE, 1),
      BI_MATCH_IF(EQ, BIND_PCI_TOPO, kPciBindTopo),
  };

  const device_fragment_part_t kPciFragment[] = {
      {std::size(kPciFragmentMatch), kPciFragmentMatch},
  };

  const zx_bind_inst_t kAcpiFragmentMatch[] = {
      BI_ABORT_IF(NE, BIND_PROTOCOL, bind_fuchsia_acpi::BIND_PROTOCOL_DEVICE),
      BI_ABORT_IF(NE, BIND_ACPI_BUS_TYPE, bind_fuchsia_acpi::BIND_ACPI_BUS_TYPE_PCI),
      BI_MATCH_IF(EQ, BIND_PCI_TOPO, kPciBindTopo),
  };

  const device_fragment_part_t kAcpiFragment[] = {
      {std::size(kAcpiFragmentMatch), kAcpiFragmentMatch},
  };

  // These are laid out so that ACPI can be optionally included via the number
  // of fragments specified.
  const device_fragment_t kFragments[] = {
      {"pci", std::size(kPciFragment), kPciFragment},
      {"sysmem", std::size(kSysmemFragment), kSysmemFragment},
      {"acpi", std::size(kAcpiFragment), kAcpiFragment},
  };

  composite_device_desc_t desc = composite_device_desc_t{
      .props = kPciDeviceProps,
      .props_count = std::size(kPciDeviceProps),
      .fragments = kFragments,
      .fragments_count = info.has_acpi ? std::size(kFragments) : std::size(kFragments) - 1,
      .primary_fragment = "pci",
      .spawn_colocated = false,
  };
  return device_add_composite(pci_dev_parent, composite_name, &desc);
}

ddk::CompositeNodeSpec CreateCompositeNodeSpec(const CompositeInfo& info) {
  auto kPciBindTopo =
      static_cast<uint32_t>(BIND_PCI_TOPO_PACK(info.bus_id, info.dev_id, info.func_id));

  const ddk::BindRule kSysmemRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_sysmem::BIND_FIDL_PROTOCOL_DEVICE),
  };

  const device_bind_prop_t kSysmemProperties[] = {
      ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                        bind_fuchsia_sysmem::BIND_FIDL_PROTOCOL_DEVICE),
  };

  const ddk::BindRule kAcpiRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_acpi::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::ACPI_BUS_TYPE,
                              bind_fuchsia_acpi::BIND_ACPI_BUS_TYPE_PCI),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_TOPO, kPciBindTopo),
  };

  const device_bind_prop_t kAcpiProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_acpi::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia::ACPI_BUS_TYPE, bind_fuchsia_acpi::BIND_ACPI_BUS_TYPE_PCI),
      ddk::MakeProperty(bind_fuchsia::PCI_TOPO, kPciBindTopo),
  };

  const ddk::BindRule kPciRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_pci::BIND_FIDL_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_VID, info.vendor_id),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_DID, info.device_id),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_CLASS, info.class_id),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_SUBCLASS, info.subclass),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_INTERFACE, info.program_interface),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_REVISION, info.revision_id),
      ddk::MakeAcceptBindRule(bind_fuchsia::PCI_TOPO, kPciBindTopo),
      ddk::MakeRejectBindRule(bind_fuchsia::COMPOSITE, static_cast<uint32_t>(1)),
  };

  const device_bind_prop_t kPciProperties[] = {
      ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_pci::BIND_FIDL_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia::PCI_VID, info.vendor_id),
      ddk::MakeProperty(bind_fuchsia::PCI_DID, info.device_id),
      ddk::MakeProperty(bind_fuchsia::PCI_CLASS, info.class_id),
      ddk::MakeProperty(bind_fuchsia::PCI_SUBCLASS, info.subclass),
      ddk::MakeProperty(bind_fuchsia::PCI_INTERFACE, info.program_interface),
      ddk::MakeProperty(bind_fuchsia::PCI_REVISION, info.revision_id),
      ddk::MakeProperty(bind_fuchsia::PCI_TOPO, kPciBindTopo),
  };

  auto composite_node_spec = ddk::CompositeNodeSpec(kSysmemRules, kSysmemProperties)
                                 .AddParentSpec(kPciRules, kPciProperties);
  if (info.has_acpi) {
    composite_node_spec.AddParentSpec(kAcpiRules, kAcpiProperties);
  }
  return composite_node_spec;
}

}  // namespace pci
