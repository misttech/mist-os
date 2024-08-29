// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "stack.h"

#include <lib/stdcompat/span.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <numeric>

#include "util.h"

const size_t kRandomSeedBytes = 16;

size_t get_initial_stack_size(
    const std::string_view& path, const fbl::static_vector<std::string_view, kMaxInitArgs>& argv,
    const fbl::static_vector<std::string_view, kMaxInitArgs>& environ,
    const fbl::static_vector<std::pair<uint32_t, uint64_t>, kMaxAuvxSize>& auxv) {
  auto accumulate_size = [](size_t accumulator, const auto& arg) {
    return accumulator + arg.length() + 1;
  };

  size_t stack_size = std::accumulate(argv.begin(), argv.end(), 0, accumulate_size);
  stack_size += std::accumulate(environ.begin(), environ.end(), 0, accumulate_size);
  stack_size += path.length() + 1;
  stack_size += kRandomSeedBytes;
  stack_size += ((argv.size() + 1) + (environ.size() + 1)) * sizeof(const char*);
  stack_size += auxv.size() * 2 * sizeof(uint64_t);
  return stack_size;
}

zx_mistos_process_stack_t populate_initial_stack(
    const zx::debuglog& log, zx::vmo& stack_vmo, const std::string_view& path,
    const fbl::static_vector<std::string_view, kMaxInitArgs>& argv,
    const fbl::static_vector<std::string_view, kMaxInitEnvs>& envp,
    fbl::static_vector<std::pair<uint32_t, uint64_t>, kMaxAuvxSize>& auxv, zx_vaddr_t stack_base,
    zx_vaddr_t original_stack_start_addr) {
  auto stack_pointer = original_stack_start_addr;

  auto write_stack = [&](const cpp20::span<const uint8_t>& data, zx_vaddr_t addr) -> zx_status_t {
    printl(log, "write [%lx] - %p - %zu\n", addr, data.data(), data.size());
    return stack_vmo.write(data.data(), (addr - stack_base), data.size());
  };

  auto argv_end = stack_pointer;
  for (auto iter = argv.rbegin(); iter != argv.rend(); ++iter) {
    cpp20::span<const uint8_t> arg{reinterpret_cast<const uint8_t*>(iter->data()),
                                   iter->length() + 1};

    stack_pointer -= arg.size();
    auto result = write_stack(arg, stack_pointer);
    check(log, result, "failed to write arg to stack");
  }
  auto argv_start = stack_pointer;

  auto environ_end = stack_pointer;
  for (auto iter = envp.rbegin(); iter != envp.rend(); ++iter) {
    cpp20::span<const uint8_t> env{reinterpret_cast<const uint8_t*>(iter->data()),
                                   iter->length() + 1};
    stack_pointer -= env.size();
    auto result = write_stack(env, stack_pointer);
    check(log, result, "failed to write env to stack");
  }
  auto environ_start = stack_pointer;

  // Write the path used with execve.
  stack_pointer -= path.length() + 1;
  auto execfn_addr = stack_pointer;
  auto result =
      write_stack({reinterpret_cast<const uint8_t*>(path.data()), path.length() + 1}, execfn_addr);
  check(log, result, "failed to write execfn to stack");

  std::array<uint8_t, kRandomSeedBytes> random_seed{};
  zx_cprng_draw(random_seed.data(), random_seed.size());
  stack_pointer -= random_seed.size();
  auto random_seed_addr = stack_pointer;
  result = write_stack({random_seed.data(), random_seed.size()}, random_seed_addr);
  check(log, result, "failed to write random seed to stack");
  stack_pointer = random_seed_addr;

  auxv.push_back(std::pair(AT_EXECFN, static_cast<uint64_t>(execfn_addr)));
  auxv.push_back(std::pair(AT_RANDOM, static_cast<uint64_t>(random_seed_addr)));
  auxv.push_back(std::pair(AT_NULL, static_cast<uint64_t>(0)));

  // After the remainder (argc/argv/environ/auxv) is pushed, the stack pointer must be 16 byte
  // aligned. This is required by the ABI and assumed by the compiler to correctly align SSE
  // operations. But this can't be done after it's pushed, since it has to be right at the top
  // of the stack. So we collect it all, align the stack appropriately now that we know the
  // size, and push it all at once.
  fbl::static_vector<uint8_t, sizeof(intptr_t) + (kMaxInitArgs * sizeof(intptr_t)) +
                                  (kMaxInitEnvs * sizeof(intptr_t)) + kMaxAuvxSize>
      main_data;

  // argc
  uint64_t argc = argv.size();
  cpp20::span<uint8_t> argc_data(reinterpret_cast<uint8_t*>(&argc), sizeof(argc));
  std::copy_n(argc_data.data(), argc_data.size(), std::back_inserter(main_data));

  // argv
  constexpr fbl::static_vector<uint8_t, 8> kZero(8, 0u);
  auto next_arg_addr = argv_start;
  for (auto arg : argv) {
    cpp20::span<uint8_t> ptr(reinterpret_cast<uint8_t*>(&next_arg_addr), sizeof(next_arg_addr));
    std::copy_n(ptr.data(), ptr.size(), std::back_inserter(main_data));
    next_arg_addr += arg.length() + 1;
  }
  std::copy(kZero.begin(), kZero.end(), std::back_inserter(main_data));

  // environ
  auto next_env_addr = environ_start;
  for (auto env : envp) {
    cpp20::span<uint8_t> ptr(reinterpret_cast<uint8_t*>(&next_env_addr), sizeof(next_env_addr));
    std::copy_n(ptr.data(), ptr.size(), std::back_inserter(main_data));
    next_env_addr += env.length() + 1;
  }
  std::copy(kZero.begin(), kZero.end(), std::back_inserter(main_data));

  // auxv
  size_t auxv_start_offset = main_data.size();
  for (auto kv : auxv) {
    uint64_t key = static_cast<uint64_t>(kv.first);
    cpp20::span<uint8_t> key_span(reinterpret_cast<uint8_t*>(&key), sizeof(key));
    cpp20::span<uint8_t> value_span(reinterpret_cast<uint8_t*>(&kv.second), sizeof(kv.second));

    std::copy_n(key_span.data(), key_span.size(), std::back_inserter(main_data));
    std::copy_n(value_span.data(), value_span.size(), std::back_inserter(main_data));
  }
  size_t auxv_end_offset = main_data.size();

  // Time to push.
  stack_pointer -= main_data.size();
  stack_pointer -= stack_pointer % 16;
  result = write_stack(main_data, stack_pointer);
  check(log, result, "failed to write main data to stack");

  auto auxv_start = stack_pointer + auxv_start_offset;
  auto auxv_end = stack_pointer + auxv_end_offset;

  return zx_mistos_process_stack_t{
      stack_pointer, auxv_start, auxv_end, argv_start, argv_end, environ_start, environ_end,
  };
}
