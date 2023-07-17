// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/zbi-boot.h>
#include <lib/boot-options/boot-options.h>
#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/memalloc/range.h>
#include <lib/zbitl/view.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>

#include <ktl/span.h>
#include <ktl/string_view.h>
#include <phys/allocation.h>
#include <phys/boot-shim/devicetree.h>
#include <phys/boot-zbi.h>
#include <phys/main.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>

namespace {

constexpr const char* kShimName = "devicetree-boot-shim";
constexpr const char* kBootstrapShimName = "devicetree-bootstrap-boot-shim";

std::array<memalloc::Range, kDevicetreeMaxMemoryRanges> gMemoryStorage;

}  // namespace

void PhysMain(void* flat_devicetree_blob, arch::EarlyTicks ticks) {
  InitStdout();
  ApplyRelocations();
  static BootOptions boot_opts;

  devicetree::ByteView fdt_blob(static_cast<const uint8_t*>(flat_devicetree_blob),
                                std::numeric_limits<uintptr_t>::max());

  boot_shim::DevicetreeBootShim<boot_shim::DevicetreeMemoryItem,
                                boot_shim::DevicetreeBootstrapChosenNodeItem<>>
      bootstrap_shim(kBootstrapShimName, devicetree::Devicetree(fdt_blob));

  auto& memory_item = bootstrap_shim.Get<boot_shim::DevicetreeMemoryItem>();
  memory_item.InitStorage(gMemoryStorage);
  bootstrap_shim.Init();

  auto& chosen_item = bootstrap_shim.Get<boot_shim::DevicetreeBootstrapChosenNodeItem<>>();

  DevicetreeInitUart(chosen_item, boot_opts);
  gBootOptions = &boot_opts;
  MainSymbolize symbolize(kShimName);

  DevicetreeInitMemory(chosen_item, memory_item);

  // Memory has been initialized, we can finish up parsing the rest of the items from the boot shim.
  // The list here is architecture dependant, and should be factored out eventually.
  boot_shim::DevicetreeBootShim<boot_shim::ArmDevicetreePsciItem, boot_shim::ArmDevicetreeGicItem>
      shim(kShimName, bootstrap_shim.devicetree());
  // TODO(fxbug.dev/129729): handoff to uart item to parse interrupts.
  if (chosen_item.cmdline()) {
    shim.set_cmdline(*chosen_item.cmdline());
  }
  shim.Init();

  // Generate next data zbi so we can append relevant items from the devicetree before handing it
  // off to BootZbi.
  auto load_result =
      DevicetreeLoadZbi(ktl::string_view(reinterpret_cast<const char*>(chosen_item.zbi().data()),
                                         chosen_item.zbi().size()),
                        bootstrap_shim, shim);
  if (load_result.is_error()) {
    printf("%*s\n", static_cast<int>(load_result.error_value().length()),
           load_result.error_value().data());
  }

  zbitl::Image<Allocation> new_zbi = std::move(*load_result);

  // Use the generated zbi to do some setup.
  ArchSetUp(new_zbi.storage()->data());

  // Finally we can boot into the kernel image.
  BootZbi::InputZbi zbi_view(new_zbi.storage().data());
  BootZbi boot;

  if (shim.Check("Not a bootable ZBI", boot.Init(zbi_view)) &&
      shim.Check("Failed to load ZBI", boot.Load())) {
    boot.Log();
    boot.Boot();
  }
  __UNREACHABLE;
}
