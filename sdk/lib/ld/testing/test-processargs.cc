// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/ld/testing/test-processargs.h"

#include <lib/stdcompat/span.h>
#include <lib/zx/channel.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <zircon/dlfcn.h>
#include <zircon/processargs.h>
#include <zircon/syscalls.h>

#include <array>
#include <functional>
#include <numeric>

#include <gtest/gtest.h>

namespace ld::testing {

TestProcessArgs& TestProcessArgs::AddHandle(uint32_t info, zx::handle handle) {
  handles_.push_back(std::move(handle));
  handle_info_.push_back(info);
  return *this;
}

TestProcessArgs& TestProcessArgs::AddDuplicateHandle(uint32_t info, zx::unowned_handle ref) {
  zx::handle handle;
  EXPECT_EQ(ref->duplicate(ZX_RIGHT_SAME_RIGHTS, &handle), ZX_OK);
  return AddHandle(info, std::move(handle));
}

TestProcessArgs& TestProcessArgs::AddFd(int fd, zx::handle handle) {
  return AddHandle(PA_HND(PA_FD, fd), std::move(handle));
}

TestProcessArgs& TestProcessArgs::AddName(std::string_view name, uint32_t info,
                                          zx::channel handle) {
  return AddHandle(PA_HND(info, static_cast<uint16_t>(next_name())), std::move(handle))
      .AddName(name);
}

TestProcessArgs& TestProcessArgs::AddInProcessTestHandles() {
  return AddDuplicateHandle(PA_PROC_SELF, zx::process::self())
      .AddDuplicateHandle(PA_THREAD_SELF, zx::thread::self());
}

TestProcessArgs& TestProcessArgs::AddLdsvc(zx::channel ldsvc) {
  if (!ldsvc) {
    EXPECT_EQ(dl_clone_loader_service(ldsvc.reset_and_get_address()), ZX_OK);
  }
  return AddHandle(PA_LDSVC_LOADER, std::move(ldsvc));
}

void TestProcessArgs::PackBootstrap(zx::unowned_channel bootstrap_sender) {
  ASSERT_EQ(handles_.size(), handle_info_.size());

  auto on_strings = [this](auto&&... args) {
    return std::invoke(std::forward<decltype(args)>(args)..., args_, env_, names_);
  };

  auto [args_size, env_size, names_size] = on_strings([](auto&&... list) {
    constexpr auto sum_strings = [](uint32_t total, std::string_view str) {
      return total + static_cast<uint32_t>(str.size()) + 1;
    };
    return std::array{std::accumulate(list.begin(), list.end(), uint32_t{}, sum_strings)...};
  });

  constexpr uint32_t info_off = sizeof(zx_proc_args_t);
  const uint32_t args_off = info_off + static_cast<uint32_t>(handles_.size() * sizeof(uint32_t));
  const uint32_t env_off = args_off + args_size;
  const uint32_t names_off = env_off + env_size;
  const uint32_t message_size = names_off + names_size;

  std::unique_ptr<char[]> buffer{new char[message_size]};
  new (buffer.get()) zx_proc_args_t{
      .protocol = ZX_PROCARGS_PROTOCOL,
      .version = ZX_PROCARGS_VERSION,
      .handle_info_off = info_off,
      .args_off = args_off,
      .args_num = static_cast<uint32_t>(args_.size()),
      .environ_off = env_off,
      .environ_num = static_cast<uint32_t>(env_.size()),
      .names_off = names_off,
      .names_num = static_cast<uint32_t>(names_.size()),
  };

  uint32_t* info = reinterpret_cast<uint32_t*>(buffer.get() + info_off);
  cpp20::span strings{
      buffer.get() + args_off,
      args_size + env_size + names_size,
  };

  std::copy(handle_info_.begin(), handle_info_.end(), info);

  on_strings([&strings](auto&&... list) mutable {
    auto add_strings = [&strings](auto&& list) {
      for (const auto& str : list) {
        strings = strings.subspan(str.copy(strings.data(), strings.size()));
        strings.front() = '\0';
        strings = strings.subspan(1);
      }
    };
    (add_strings(list), ...);
  });
  ASSERT_TRUE(strings.empty());

  std::vector<zx_handle_t> handles;
  handles.reserve(handles_.size());
  for (auto& handle : handles_) {
    handles.push_back(handle.release());
  }

  handles_.clear();
  handle_info_.clear();
  args_.clear();
  env_.clear();
  names_.clear();

  ASSERT_EQ(bootstrap_sender->write(0, buffer.get(), message_size, handles.data(),
                                    static_cast<uint32_t>(handles.size())),
            ZX_OK);
}

zx::channel TestProcessArgs::PackBootstrap() {
  zx::channel bootstrap;
  if (!bootstrap_sender_) {
    zx_status_t status = zx::channel::create(0, &bootstrap_sender_, &bootstrap);
    EXPECT_EQ(status, ZX_OK);
  }
  PackBootstrap(bootstrap_sender_.borrow());
  return bootstrap;
}

}  // namespace ld::testing
