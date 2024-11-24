// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/memory/metrics/capture_strategy.h"

#include <lib/syslog/cpp/macros.h>
#include <string.h>
#include <zircon/errors.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/developer/memory/metrics/capture.h"
#include "src/developer/memory/metrics/tests/test_utils.h"

using testing::AllOf;
using testing::Field;
using testing::Matcher;
using testing::Pair;
using testing::UnorderedElementsAre;
using testing::UnorderedElementsAreArray;

namespace memory {
// Printers for a nicer output when expectations fail.
std::ostream& operator<<(std::ostream& os, const std::vector<zx_koid_t>& vmos) {
  os << "[";
  for (const auto& koid : vmos) {
    os << koid << ", ";
  }
  os << "]";
  return os;
}

std::ostream& operator<<(std::ostream& os, const Process& proc) {
  os << "Process{.job: " << proc.job << ", .koid: " << proc.koid << ", .name: " << proc.name
     << ", .vmos: " << proc.vmos << "}";
  return os;
}

std::ostream& operator<<(std::ostream& os, const zx_info_vmo_t& vmo) {
  os << "zx_info_vmo_t{.koid: " << vmo.koid << ", .parent_koid: " << vmo.parent_koid
     << ", .name: " << vmo.name << ", .committed_bytes: " << vmo.committed_bytes << "}";
  return os;
}

std::ostream& operator<<(std::ostream& os, const Vmo& vmo) {
  os << "Vmo{.koid: " << vmo.koid << ", .parent_koid: " << vmo.parent_koid
     << ", .name: " << vmo.name << ", .committed_bytes: " << vmo.committed_bytes.integral
     << ", .allocated_bytes: " << vmo.allocated_bytes << ", .children: " << vmo.children << "}";
  return os;
}

namespace test {

memory::Process MakeProcess(zx_koid_t job, zx_koid_t process, char* name, size_t name_size) {
  memory::Process out{process, job};
  strncpy(out.name, name, name_size);
  return out;
}

const zx_koid_t koid_job_0 = 10;
const zx_koid_t koid_process_0 = 100;
const zx_handle_t handle_process_0 = 1000;
const char kProcessName_0[] = "proc_0";

const zx_koid_t koid_job_1 = 11;
const zx_koid_t koid_process_1 = 101;
const zx_handle_t handle_process_1 = 1001;
const char kProcessName_1[] = "proc_1";

const zx_koid_t koid_vmo_0 = 200;

const static zx_info_vmo_t _vmo_0 = {
    .koid = koid_vmo_0,
    .name = "vmo_0",
    .size_bytes = 0,
};
const static GetInfoResponse vmo_0_info = {
    handle_process_0, ZX_INFO_PROCESS_VMOS, &_vmo_0, sizeof(_vmo_0), 1, ZX_OK};

const zx_koid_t koid_vmo_1 = 201;

const static zx_info_vmo_t _vmo_1 = {
    .koid = koid_vmo_1,
    .name = "vmo_0",
    .size_bytes = 0,
};
const static GetInfoResponse vmo_1_info = {
    handle_process_1, ZX_INFO_PROCESS_VMOS, &_vmo_1, sizeof(_vmo_1), 1, ZX_OK};

const zx_koid_t koid_job_2 = 12;
const zx_koid_t koid_process_2 = 102;
const zx_handle_t handle_process_2 = 1002;

const zx_koid_t koid_vmo_2 = 202;
const zx_vaddr_t addr_vmo_2 = 0x402000100000;

const zx_koid_t koid_vmo_2b = 212;
// VMO 2B is not mapped.

const zx_koid_t koid_job_3 = koid_job_2;
const zx_koid_t koid_process_3 = 103;
const zx_handle_t handle_process_3 = 1003;
const char kProcessName_3[] = "proc_3";

const zx_koid_t koid_vmo_3 = 203;
const zx_vaddr_t addr_vmo_3 = 0x100002;

const static zx_info_vmo_t _vmo_2[] = {{
                                           .koid = koid_vmo_2,
                                           .name = "vmo_2",
                                           .size_bytes = 0,
                                       },
                                       {
                                           .koid = koid_vmo_2b,
                                           .name = "vmo_2b",
                                           .size_bytes = 0,
                                       },
                                       {
                                           .koid = koid_vmo_3,
                                           .name = "vmo_3",
                                           .size_bytes = 0,
                                       }};
const static GetInfoResponse vmo_2_info = {
    handle_process_2, ZX_INFO_PROCESS_VMOS, _vmo_2, sizeof(zx_info_vmo_t), 3, ZX_OK};

const static zx_info_maps_t _mappings_2[] = {{
                                                 .base = 0,
                                                 .size = 0x400000000000,
                                                 .depth = 1,
                                                 .type = ZX_INFO_MAPS_TYPE_VMAR,
                                             },
                                             {
                                                 .base = 0x400000000000,
                                                 .size = 0x400000000000,
                                                 .depth = 1,
                                                 .type = ZX_INFO_MAPS_TYPE_VMAR,
                                             },
                                             {
                                                 .base = addr_vmo_2,
                                                 .type = ZX_INFO_MAPS_TYPE_MAPPING,
                                                 .u = {.mapping = {.vmo_koid = koid_vmo_2}},
                                             }};
const static GetInfoResponse maps_2_info = {
    handle_process_2, ZX_INFO_PROCESS_MAPS, _mappings_2, sizeof(zx_info_maps_t), 3, ZX_OK};

// |vmo_3_info| should contain the same VMOs as vmo_2_info per the shared handle table of shared
// processes used by Starnix.
const static GetInfoResponse vmo_3_info = {
    handle_process_3, ZX_INFO_PROCESS_VMOS, _vmo_2, sizeof(zx_info_vmo_t), 3, ZX_OK};
const static zx_info_maps_t _mappings_3[] = {{
                                                 .base = 0,
                                                 .size = 0x400000000000,
                                                 .depth = 1,
                                                 .type = ZX_INFO_MAPS_TYPE_VMAR,
                                             },
                                             {
                                                 .base = 0x400000000000,
                                                 .size = 0x400000000000,
                                                 .depth = 1,
                                                 .type = ZX_INFO_MAPS_TYPE_VMAR,
                                             },
                                             {
                                                 .base = addr_vmo_2,
                                                 .type = ZX_INFO_MAPS_TYPE_MAPPING,
                                                 .u = {.mapping = {.vmo_koid = koid_vmo_2}},
                                             },
                                             {
                                                 .base = addr_vmo_3,
                                                 .type = ZX_INFO_MAPS_TYPE_MAPPING,
                                                 .u = {.mapping = {.vmo_koid = koid_vmo_3}},
                                             }};
const static GetInfoResponse maps_3_info = {
    handle_process_3, ZX_INFO_PROCESS_MAPS, _mappings_3, sizeof(zx_info_maps_t), 4, ZX_OK};

Matcher<Process> MakeProcessMatcher(zx_koid_t process, zx_koid_t job, std::string name,
                                    std::vector<zx_koid_t> vmos) {
  return AllOf(Field(&Process::koid, process), Field(&Process::job, job),
               Field(&Process::name, testing::StrEq(name)),
               Field(&Process::vmos, UnorderedElementsAreArray(vmos)));
}

Matcher<Vmo> MakeVmoMatcher(zx_info_vmo_t vmo) {
  return AllOf(Field(&Vmo::koid, vmo.koid), Field(&Vmo::name, testing::StrEq(vmo.name)));
}

using StarnixCaptureStrategyTest = testing::Test;

TEST_F(StarnixCaptureStrategyTest, NoStarnixProcess) {
  MockOS os({.get_info = {vmo_0_info, vmo_1_info}});
  StarnixCaptureStrategy strategy;

  memory::Process process_0{.koid = koid_process_0, .job = koid_job_0};
  std::strncpy(process_0.name, kProcessName_0, ZX_MAX_NAME_LEN);
  zx::handle handle_0(handle_process_0);

  memory::Process process_1{.koid = koid_process_1, .job = koid_job_1};
  std::strncpy(process_1.name, kProcessName_1, ZX_MAX_NAME_LEN);
  zx::handle handle_1(handle_process_1);

  strategy.OnNewProcess(os, std::move(process_0), std::move(handle_0));
  strategy.OnNewProcess(os, std::move(process_1), std::move(handle_1));
  auto result = strategy.Finalize(os);
  EXPECT_TRUE(result.is_ok());

  auto& [koid_to_process, koid_to_vmo] = result.value();

  EXPECT_THAT(
      koid_to_process,
      UnorderedElementsAre(Pair(koid_process_0, MakeProcessMatcher(koid_process_0, koid_job_0,
                                                                   kProcessName_0, {koid_vmo_0})),
                           Pair(koid_process_1, MakeProcessMatcher(koid_process_1, koid_job_1,
                                                                   kProcessName_1, {koid_vmo_1}))));

  EXPECT_THAT(koid_to_vmo, UnorderedElementsAre(Pair(koid_vmo_0, MakeVmoMatcher(_vmo_0)),
                                                Pair(koid_vmo_1, MakeVmoMatcher(_vmo_1))));
}

TEST_F(StarnixCaptureStrategyTest, WithStarnixProcess) {
  MockOS os(
      {.get_info = {vmo_0_info, vmo_1_info, vmo_2_info, maps_2_info, vmo_3_info, maps_3_info}});
  StarnixCaptureStrategy strategy;

  memory::Process process_0{.koid = koid_process_0, .job = koid_job_0};
  std::strncpy(process_0.name, kProcessName_0, ZX_MAX_NAME_LEN);
  zx::handle handle_0(handle_process_0);

  memory::Process process_1{.koid = koid_process_1, .job = koid_job_1};
  std::strncpy(process_1.name, kProcessName_1, ZX_MAX_NAME_LEN);
  zx::handle handle_1(handle_process_1);

  memory::Process process_2{.koid = koid_process_2, .job = koid_job_2};
  std::strncpy(process_2.name, STARNIX_KERNEL_PROCESS_NAME, ZX_MAX_NAME_LEN);
  zx::handle handle_2(handle_process_2);

  memory::Process process_3{.koid = koid_process_3, .job = koid_job_3};
  std::strncpy(process_3.name, kProcessName_3, ZX_MAX_NAME_LEN);
  zx::handle handle_3(handle_process_3);

  strategy.OnNewProcess(os, std::move(process_0), std::move(handle_0));
  strategy.OnNewProcess(os, std::move(process_1), std::move(handle_1));
  strategy.OnNewProcess(os, std::move(process_2), std::move(handle_2));
  strategy.OnNewProcess(os, std::move(process_3), std::move(handle_3));
  auto result = strategy.Finalize(os);
  EXPECT_TRUE(result.is_ok());

  auto& [koid_to_process, koid_to_vmo] = result.value();

  EXPECT_THAT(
      koid_to_process,
      UnorderedElementsAre(Pair(koid_process_0, MakeProcessMatcher(koid_process_0, koid_job_0,
                                                                   kProcessName_0, {koid_vmo_0})),
                           Pair(koid_process_1, MakeProcessMatcher(koid_process_1, koid_job_1,
                                                                   kProcessName_1, {koid_vmo_1})),
                           Pair(koid_process_2, MakeProcessMatcher(koid_process_2, koid_job_2,
                                                                   STARNIX_KERNEL_PROCESS_NAME,
                                                                   {koid_vmo_2, koid_vmo_2b})),
                           Pair(koid_process_3, MakeProcessMatcher(koid_process_3, koid_job_3,
                                                                   kProcessName_3, {koid_vmo_3}))));

  EXPECT_THAT(koid_to_vmo, UnorderedElementsAre(Pair(koid_vmo_0, MakeVmoMatcher(_vmo_0)),
                                                Pair(koid_vmo_1, MakeVmoMatcher(_vmo_1)),
                                                Pair(koid_vmo_2, MakeVmoMatcher(_vmo_2[0])),
                                                Pair(koid_vmo_2b, MakeVmoMatcher(_vmo_2[1])),
                                                Pair(koid_vmo_3, MakeVmoMatcher(_vmo_2[2]))));
}

}  // namespace test
}  // namespace memory
