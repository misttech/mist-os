// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpt.metadata/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/sysmem/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <zircon/hw/gpt.h>

#include "x86.h"

namespace x86 {

namespace fpbus = fuchsia_hardware_platform_bus;

// This value is passed to bti_create as a marker; it does not have a particular
// meaning to anything in the system.
#define SYSMEM_BTI_ID 0x12341234UL

static const std::vector<fpbus::Bti> sysmem_btis = {
    {{
        .iommu_index = 0,
        .bti_id = SYSMEM_BTI_ID,
    }},
};

// On x86 not much is known about the display adapter or other hardware.
static const sysmem_metadata_t sysmem_metadata = {
    .vid = PDEV_VID_GENERIC,
    .pid = PDEV_PID_GENERIC,
    // no protected pool
    .protected_memory_size = 0,
    // -5 means 5% of physical RAM
    // we allocate a small amount of contiguous RAM to keep the sysmem tests from flaking,
    // see https://fxbug.dev/67703.
    .contiguous_memory_size = -5,
};

static zx_status_t GetDeviceMetadataGpt(std::vector<uint8_t> &device_metadata_gpt_encoded) {
  fidl::Arena fidl_arena;
  fidl::VectorView<fuchsia_hardware_gpt_metadata::wire::PartitionInfo> partition_info(fidl_arena,
                                                                                      2);

  // Block core should ignore the 'misc' partition; the abr-shim driver will bind in its place.
  partition_info[0].name = GUID_ABR_META_NAME;
  partition_info[0].options =
      fuchsia_hardware_gpt_metadata::wire::PartitionOptions::Builder(fidl_arena)
          .type_guid_override(GUID_ABR_META_VALUE)
          .block_driver_should_ignore_device(true)
          .Build();

  partition_info[1].name = GPT_DURABLE_BOOT_NAME;
  partition_info[1].options =
      fuchsia_hardware_gpt_metadata::wire::PartitionOptions::Builder(fidl_arena)
          .type_guid_override(GPT_DURABLE_BOOT_TYPE_GUID)
          .block_driver_should_ignore_device(true)
          .Build();

  fit::result encoded =
      fidl::Persist(fuchsia_hardware_gpt_metadata::wire::GptInfo::Builder(fidl_arena)
                        .partition_info(partition_info)
                        .Build());

  if (!encoded.is_ok()) {
    zxlogf(ERROR, "Failed to encode GPT metadata: %s",
           encoded.error_value().FormatDescription().c_str());
    return encoded.error_value().status();
  }

  device_metadata_gpt_encoded = std::move(encoded.value());
  return ZX_OK;
}

static const std::vector<fpbus::Metadata> GetSysmemMetadataList(
    std::vector<uint8_t> device_metadata_gpt) {
  return std::vector<fpbus::Metadata>{
      {{
          .type = SYSMEM_METADATA_TYPE,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t *>(&sysmem_metadata),
              reinterpret_cast<const uint8_t *>(&sysmem_metadata) + sizeof(sysmem_metadata)),
      }},
      {{
          .type = DEVICE_METADATA_GPT_INFO,
          .data = std::move(device_metadata_gpt),
      }}};
}

static const fpbus::Node GetSystemDev(std::vector<uint8_t> device_metadata_gpt) {
  fpbus::Node node;
  node.name() = "sysmem";
  node.vid() = PDEV_VID_GENERIC;
  node.pid() = PDEV_PID_GENERIC;
  node.did() = PDEV_DID_SYSMEM;
  node.bti() = sysmem_btis;
  node.metadata() = GetSysmemMetadataList(std::move(device_metadata_gpt));
  return node;
}

zx_status_t X86::SysmemInit() {
  std::vector<uint8_t> device_metadata_gpt;
  auto res = GetDeviceMetadataGpt(device_metadata_gpt);
  if (res != ZX_OK) {
    return res;
  }

  auto sysmem_dev = GetSystemDev(std::move(device_metadata_gpt));

  fdf::Arena arena('X86S');
  fidl::Arena fidl_arena;
  auto status = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, sysmem_dev));
  if (!status.ok()) {
    zxlogf(ERROR, "%s: NodeAdd failed %s", __func__, status.FormatDescription().data());
    return status.status();
  }
  if (status->is_error()) {
    return status->error_value();
  }

  return ZX_OK;
}
}  // namespace x86
