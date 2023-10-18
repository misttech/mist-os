// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/fit/defer.h>
#include <lib/zbitl/image.h>

#include "devicetree-test-fixture.h"
#include "lib/zbi-format/cpu.h"

namespace {
using devicetree_test::LoadDtb;
using devicetree_test::LoadedDtb;

class RiscvDevictreeCpuTopologyItemTest
    : public devicetree_test::TestMixin<devicetree_test::RiscvDevicetreeTest> {
 public:
  static void SetUpTestSuite() {
    TestMixin<RiscvDevicetreeTest>::SetUpTestSuite();
    auto loaded_dtb = LoadDtb("cpus_riscv.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    riscv_cpus_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("cpus_riscv_nested_clusters.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    riscv_cpus_nested_clusters_dtb_ = std::move(loaded_dtb).value();

    loaded_dtb = LoadDtb("cpus_no_cpu_map_riscv.dtb");
    ASSERT_TRUE(loaded_dtb.is_ok(), "%s", loaded_dtb.error_value().c_str());
    riscv_cpus_no_cpu_map_dtb_ = std::move(loaded_dtb).value();
  }

  static void TearDownTestSuite() {
    riscv_cpus_dtb_ = std::nullopt;
    riscv_cpus_no_cpu_map_dtb_ = std::nullopt;
    TestMixin<RiscvDevicetreeTest>::TearDownTestSuite();
  }

  devicetree::Devicetree riscv_cpus() { return riscv_cpus_dtb_->fdt(); }
  devicetree::Devicetree riscv_cpus_nested_clusters() {
    return riscv_cpus_nested_clusters_dtb_->fdt();
  }
  devicetree::Devicetree riscv_cpus_no_cpu_map() { return riscv_cpus_no_cpu_map_dtb_->fdt(); }

 private:
  static std::optional<LoadedDtb> riscv_cpus_dtb_;
  static std::optional<LoadedDtb> riscv_cpus_nested_clusters_dtb_;
  static std::optional<LoadedDtb> riscv_cpus_no_cpu_map_dtb_;
};

std::optional<LoadedDtb> RiscvDevictreeCpuTopologyItemTest::riscv_cpus_dtb_ = std::nullopt;
std::optional<LoadedDtb> RiscvDevictreeCpuTopologyItemTest::riscv_cpus_nested_clusters_dtb_ =
    std::nullopt;
std::optional<LoadedDtb> RiscvDevictreeCpuTopologyItemTest::riscv_cpus_no_cpu_map_dtb_ =
    std::nullopt;

TEST_F(RiscvDevictreeCpuTopologyItemTest, CpusWithCpuMap) {
  constexpr std::array kExpectedTopology = {

      // socket0
      zbi_topology_node_t{
          .entity = {.discriminant = ZBI_TOPOLOGY_ENTITY_SOCKET},
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },

      // cluster0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_CLUSTER,
                  .cluster =
                      {
                          .performance_class = 0xFF,
                      },
              },
          .parent_index = 0,
      },

      // cpu@0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 0,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {3, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 1,
      },

      // cpu@1
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 1,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {1, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 1,
      },

      // cluster1
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_CLUSTER,
                  .cluster =
                      {
                          .performance_class = 0x7F,
                      },
              },
          .parent_index = 0,
      },

      // cpu@2
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 2,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {2, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 4,
      },

      // cpu@3
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 3,
                                      },
                              },
                          .flags = ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY,
                          .logical_ids = {0, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 4,
      },
  };

  std::array<std::byte, 1024> image_buffer;
  std::vector<void*> allocs;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = riscv_cpus();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevictreeCpuTopologyItem> shim("test", fdt);
  shim.set_allocator([&allocs](size_t size, size_t alignment) -> void* {
    // Custom aligned_alloc since OS X doesnt support it in some versions.
    void* alloc = malloc(size + alignment);
    allocs.push_back(alloc);
    return reinterpret_cast<void*>((reinterpret_cast<uintptr_t>(alloc) + alignment) &
                                   ~(alignment - 1));
  });
  shim.Get<boot_shim::RiscvDevictreeCpuTopologyItem>().set_boot_hart_id(3);

  auto release_memory = fit::defer([&]() {
    for (auto* alloc : allocs) {
      free(alloc);
    }
  });

  shim.Init();
  auto clear_errors = fit::defer([&]() { image.ignore_error(); });
  ASSERT_TRUE(shim.AppendItems(image).is_ok());
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_CPU_TOPOLOGY) {
      present = true;
      cpp20::span<zbi_topology_node_t> nodes(reinterpret_cast<zbi_topology_node_t*>(payload.data()),
                                             payload.size() / sizeof(zbi_topology_node_t));
      devicetree_test::CheckCpuTopology(nodes, kExpectedTopology);
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevictreeCpuTopologyItemTest, CpuNodesWithNestedClusters) {
  constexpr std::array kExpectedTopology = {

      // socket0
      zbi_topology_node_t{
          .entity = {.discriminant = ZBI_TOPOLOGY_ENTITY_SOCKET},
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },

      // cluster0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_CLUSTER,
                  .cluster =
                      {
                          .performance_class = 0xFF,
                      },
              },
          .parent_index = 0,
      },

      // cluster0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_CLUSTER,
                  .cluster =
                      {
                          .performance_class = 0xFF,
                      },
              },
          .parent_index = 1,
      },

      // cluster0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_CLUSTER,
                  .cluster =
                      {
                          .performance_class = 0xFF,
                      },
              },
          .parent_index = 2,
      },

      // cpu@0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 0,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {3, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 3,
      },

      // cpu@1
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 1,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {1, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 3,
      },

      // cluster1
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_CLUSTER,
                  .cluster =
                      {
                          .performance_class = 0x7F,
                      },
              },
          .parent_index = 0,
      },

      // cluster0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_CLUSTER,
                  .cluster =
                      {
                          .performance_class = 0x7F,
                      },
              },
          .parent_index = 6,
      },

      // cluster2
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_CLUSTER,
                  .cluster =
                      {
                          .performance_class = 0x7F,
                      },
              },
          .parent_index = 7,
      },

      // cpu@2
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 2,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {2, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 8,
      },

      // cpu@3
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 3,
                                      },
                              },
                          .flags = ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY,
                          .logical_ids = {0, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 8,
      },
  };

  std::array<std::byte, 1024> image_buffer;
  std::vector<void*> allocs;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = riscv_cpus_nested_clusters();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevictreeCpuTopologyItem> shim("test", fdt);
  shim.set_allocator([&allocs](size_t size, size_t alignment) -> void* {
    // Custom aligned_alloc since OS X doesnt support it in some versions.
    void* alloc = malloc(size + alignment);
    allocs.push_back(alloc);
    return reinterpret_cast<void*>((reinterpret_cast<uintptr_t>(alloc) + alignment) &
                                   ~(alignment - 1));
  });
  shim.Get<boot_shim::RiscvDevictreeCpuTopologyItem>().set_boot_hart_id(3);

  auto release_memory = fit::defer([&]() {
    for (auto* alloc : allocs) {
      free(alloc);
    }
  });

  shim.Init();
  auto clear_errors = fit::defer([&]() { image.ignore_error(); });
  ASSERT_TRUE(shim.AppendItems(image).is_ok());
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_CPU_TOPOLOGY) {
      present = true;
      cpp20::span<zbi_topology_node_t> nodes(reinterpret_cast<zbi_topology_node_t*>(payload.data()),
                                             payload.size() / sizeof(zbi_topology_node_t));
      devicetree_test::CheckCpuTopology(nodes, kExpectedTopology);
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevictreeCpuTopologyItemTest, CpuNodesWithoutCpuMap) {
  constexpr std::array kExpectedTopology = {
      // cpu@0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 0,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {3, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },

      // cpu@1
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 1,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {1, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },

      // cpu@2
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 2,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {2, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },

      // cpu@3
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 3,
                                      },
                              },
                          .flags = ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY,
                          .logical_ids = {0, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },
  };

  std::array<std::byte, 1024> image_buffer;
  std::vector<void*> allocs;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = riscv_cpus_no_cpu_map();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevictreeCpuTopologyItem> shim("test", fdt);
  shim.set_allocator([&allocs](size_t size, size_t alignment) -> void* {
    // Custom aligned_alloc since OS X doesnt support it in some versions.
    void* alloc = malloc(size + alignment);
    allocs.push_back(alloc);
    return reinterpret_cast<void*>((reinterpret_cast<uintptr_t>(alloc) + alignment) &
                                   ~(alignment - 1));
  });
  shim.Get<boot_shim::RiscvDevictreeCpuTopologyItem>().set_boot_hart_id(3);

  auto release_memory = fit::defer([&]() {
    for (auto* alloc : allocs) {
      free(alloc);
    }
  });

  shim.Init();
  auto clear_errors = fit::defer([&]() { image.ignore_error(); });
  ASSERT_TRUE(shim.AppendItems(image).is_ok());
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_CPU_TOPOLOGY) {
      present = true;
      cpp20::span<zbi_topology_node_t> nodes(reinterpret_cast<zbi_topology_node_t*>(payload.data()),
                                             payload.size() / sizeof(zbi_topology_node_t));
      devicetree_test::CheckCpuTopology(nodes, kExpectedTopology);
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevictreeCpuTopologyItemTest, Qemu) {
  constexpr std::array kExpectedTopology = {
      // cluster0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_CLUSTER,
                  .cluster =
                      {
                          .performance_class = 0x1,
                      },
              },
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },

      // cpu@0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 0,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {3, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 0,
      },

      // cpu@1
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 1,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {1, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 0,
      },

      // cpu@2
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 2,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {2, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 0,
      },

      // cpu@3
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 3,
                                      },
                              },
                          .flags = ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY,
                          .logical_ids = {0, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 0,
      },
  };

  std::array<std::byte, 1024> image_buffer;
  std::vector<void*> allocs;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = qemu_riscv();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevictreeCpuTopologyItem> shim("test", fdt);
  shim.set_allocator([&allocs](size_t size, size_t alignment) -> void* {
    // Custom aligned_alloc since OS X doesnt support it in some versions.
    void* alloc = malloc(size + alignment);
    allocs.push_back(alloc);
    return reinterpret_cast<void*>((reinterpret_cast<uintptr_t>(alloc) + alignment) &
                                   ~(alignment - 1));
  });
  shim.Get<boot_shim::RiscvDevictreeCpuTopologyItem>().set_boot_hart_id(3);

  auto release_memory = fit::defer([&]() {
    for (auto* alloc : allocs) {
      free(alloc);
    }
  });

  shim.Init();
  auto clear_errors = fit::defer([&]() { image.ignore_error(); });
  ASSERT_TRUE(shim.AppendItems(image).is_ok());
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_CPU_TOPOLOGY) {
      present = true;
      cpp20::span<zbi_topology_node_t> nodes(reinterpret_cast<zbi_topology_node_t*>(payload.data()),
                                             payload.size() / sizeof(zbi_topology_node_t));
      devicetree_test::CheckCpuTopology(nodes, kExpectedTopology);
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevictreeCpuTopologyItemTest, VisionFive2) {
  constexpr std::array kExpectedTopology = {
      // cpu@0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 0,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {3, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },

      // cpu@1
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 1,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {1, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },

      // cpu@2
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 2,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {2, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },

      // cpu@3
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 3,
                                      },
                              },
                          .flags = ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY,
                          .logical_ids = {0, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },

      // cpu@4
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 4,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {4, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },
  };

  std::array<std::byte, 1024> image_buffer;
  std::vector<void*> allocs;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = vision_five_2();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevictreeCpuTopologyItem> shim("test", fdt);
  shim.set_allocator([&allocs](size_t size, size_t alignment) -> void* {
    // Custom aligned_alloc since OS X doesnt support it in some versions.
    void* alloc = malloc(size + alignment);
    allocs.push_back(alloc);
    return reinterpret_cast<void*>((reinterpret_cast<uintptr_t>(alloc) + alignment) &
                                   ~(alignment - 1));
  });
  shim.Get<boot_shim::RiscvDevictreeCpuTopologyItem>().set_boot_hart_id(3);

  auto release_memory = fit::defer([&]() {
    for (auto* alloc : allocs) {
      free(alloc);
    }
  });

  shim.Init();
  auto clear_errors = fit::defer([&]() { image.ignore_error(); });
  ASSERT_TRUE(shim.AppendItems(image).is_ok());
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_CPU_TOPOLOGY) {
      present = true;
      cpp20::span<zbi_topology_node_t> nodes(reinterpret_cast<zbi_topology_node_t*>(payload.data()),
                                             payload.size() / sizeof(zbi_topology_node_t));
      devicetree_test::CheckCpuTopology(nodes, kExpectedTopology);
    }
  }
  ASSERT_TRUE(present);
}

TEST_F(RiscvDevictreeCpuTopologyItemTest, HifiveSifiveUnmatched) {
  constexpr std::array kExpectedTopology = {
      // cluster0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_CLUSTER,
                  .cluster =
                      {
                          .performance_class = 0x1,
                      },
              },
          .parent_index = ZBI_TOPOLOGY_NO_PARENT,
      },

      // cpu@3
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 3,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {3, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 0,
      },

      // cpu@1
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 1,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {1, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 0,
      },

      // cpu@4
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 4,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {2, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 0,
      },

      // cpu@2
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 2,
                                      },
                              },
                          .flags = ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY,
                          .logical_ids = {0, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 0,
      },

      // cpu@0
      zbi_topology_node_t{
          .entity =
              {
                  .discriminant = ZBI_TOPOLOGY_ENTITY_PROCESSOR,
                  .processor =
                      {
                          .architecture_info =
                              {
                                  .discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64,
                                  .riscv64 =
                                      {
                                          .hart_id = 0,
                                      },
                              },
                          .flags = 0,
                          .logical_ids = {4, 0, 0, 0},
                          .logical_id_count = 1,
                      },
              },
          .parent_index = 0,
      },
  };

  std::array<std::byte, 1024> image_buffer;
  std::vector<void*> allocs;
  zbitl::Image<cpp20::span<std::byte>> image(image_buffer);
  ASSERT_TRUE(image.clear().is_ok());

  auto fdt = sifive_hifive_unmatched();
  boot_shim::DevicetreeBootShim<boot_shim::RiscvDevictreeCpuTopologyItem> shim("test", fdt);
  shim.set_allocator([&allocs](size_t size, size_t alignment) -> void* {
    // Custom aligned_alloc since OS X doesnt support it in some versions.
    void* alloc = malloc(size + alignment);
    allocs.push_back(alloc);
    return reinterpret_cast<void*>((reinterpret_cast<uintptr_t>(alloc) + alignment) &
                                   ~(alignment - 1));
  });
  shim.Get<boot_shim::RiscvDevictreeCpuTopologyItem>().set_boot_hart_id(2);

  auto release_memory = fit::defer([&]() {
    for (auto* alloc : allocs) {
      free(alloc);
    }
  });

  shim.Init();
  auto clear_errors = fit::defer([&]() { image.ignore_error(); });
  ASSERT_TRUE(shim.AppendItems(image).is_ok());
  bool present = false;
  for (auto [header, payload] : image) {
    if (header->type == ZBI_TYPE_CPU_TOPOLOGY) {
      present = true;
      cpp20::span<zbi_topology_node_t> nodes(reinterpret_cast<zbi_topology_node_t*>(payload.data()),
                                             payload.size() / sizeof(zbi_topology_node_t));
      devicetree_test::CheckCpuTopology(nodes, kExpectedTopology);
    }
  }
  ASSERT_TRUE(present);
}

}  // namespace
