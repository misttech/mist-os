// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "memory-pressure.h"

#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <stdio.h>
#include <stdlib.h>

#include <string>

#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/fxl/command_line.h"

namespace sysmem = fuchsia_sysmem;

void PrintHelp() {
  Log(""
      "Usage: sysmem-memory-pressure [--contiguous] [--help] [--heap=heap] [--usage=[cpu|vulkan]] "
      "size_bytes\n");
  Log("Options:\n");
  Log(" --help           Show this message.\n");
  Log(" --contiguous     Request physically-contiguous memory\n");
  Log(" --heap           Specifies the numeric value of the sysmem heap to request memory from. By "
      "default system ram is used.\n");
  Log(" --usage          Specifies what usage should be requested from sysmem. Vulkan is the "
      "default\n");
  Log(" size_bytes       The size of the memory in bytes.\n");
}

int MemoryPressureCommand(const fxl::CommandLine& command_line, bool sleep) {
  if (command_line.HasOption("help")) {
    PrintHelp();
    return 0;
  }

  if (command_line.positional_args().size() != 1) {
    LogError("Missing size to allocate\n");
    PrintHelp();
    return 1;
  }

  std::string size_string = command_line.positional_args()[0];
  char* endptr;
  uint64_t size = strtoull(size_string.c_str(), &endptr, 0);
  if (endptr != size_string.c_str() + size_string.size()) {
    LogError("Invalid size %s\n", size_string.c_str());
    PrintHelp();
    return 1;
  }

  sysmem::wire::HeapType heap = sysmem::wire::HeapType::kSystemRam;
  std::string heap_string;
  if (command_line.GetOptionValue("heap", &heap_string)) {
    char* endptr;
    heap = static_cast<sysmem::wire::HeapType>(strtoull(heap_string.c_str(), &endptr, 0));
    if (endptr != heap_string.c_str() + heap_string.size()) {
      LogError("Invalid heap string: %s\n", heap_string.c_str());
      return 1;
    }
  }

  bool physically_contiguous = command_line.HasOption("contiguous");

  sysmem::wire::BufferCollectionConstraints constraints;
  std::string usage;
  if (command_line.GetOptionValue("usage", &usage)) {
    if (usage == "vulkan") {
      constraints.usage.vulkan = sysmem::wire::kVulkanUsageTransferDst;
    } else if (usage == "cpu") {
      constraints.usage.cpu = sysmem::wire::kCpuUsageRead;
    } else {
      LogError("Invalid usage %s\n", usage.c_str());
      PrintHelp();
      return 1;
    }
  } else {
    constraints.usage.vulkan = sysmem::wire::kVulkanUsageTransferDst;
  }
  constraints.min_buffer_count_for_camping = 1;
  constraints.has_buffer_memory_constraints = true;
  auto& mem_constraints = constraints.buffer_memory_constraints;
  mem_constraints.physically_contiguous_required = physically_contiguous;
  mem_constraints.min_size_bytes = static_cast<uint32_t>(size);
  mem_constraints.cpu_domain_supported = true;
  mem_constraints.ram_domain_supported = true;
  mem_constraints.inaccessible_domain_supported = true;
  mem_constraints.heap_permitted_count = 1;
  mem_constraints.heap_permitted[0] = heap;
  zx::result client_end = component::Connect<sysmem::Allocator>();
  if (client_end.is_error()) {
    LogError("Failed to connect to sysmem services, error %d\n", client_end.status_value());
    return 1;
  }
  fidl::WireSyncClient sysmem_allocator{std::move(client_end.value())};
  if (const fidl::OneWayStatus status = sysmem_allocator->SetDebugClientInfo(
          fidl::StringView::FromExternal(fsl::GetCurrentProcessName()),
          fsl::GetCurrentProcessKoid());
      !status.ok()) {
    LogError("Failed to set debug client info, error %d\n", status.status());
    return 1;
  };

  auto [client_collection_channel, server_collection] =
      fidl::Endpoints<sysmem::BufferCollection>::Create();

  if (const fidl::OneWayStatus status =
          sysmem_allocator->AllocateNonSharedCollection(std::move(server_collection));
      !status.ok()) {
    LogError("Failed to allocate collection, error %d\n", status.status());
    return 1;
  }
  fidl::WireSyncClient collection(std::move(client_collection_channel));

  if (const fidl::OneWayStatus status = collection->SetName(1000000, "sysmem-memory-pressure");
      !status.ok()) {
    LogError("Failed to set collection name, error %d\n", status.status());
    return 1;
  }

  if (const fidl::OneWayStatus status = collection->SetConstraints(true, constraints);
      !status.ok()) {
    LogError("Failed to set collection constraints, error %d\n", status.status());
    return 1;
  }

  const fidl::WireResult result = collection->WaitForBuffersAllocated();
  if (!result.ok()) {
    LogError("Lost connection to sysmem services, error %d\n", result.status());
    return 1;
  }
  auto const& response = result.value();
  if (response.status != ZX_OK) {
    LogError("Allocation error %d\n", response.status);
    return 1;
  }
  Log("Allocated %ld bytes. Sleeping forever\n", size);

  if (sleep) {
    zx::nanosleep(zx::time::infinite());
  }

  return 0;
}
