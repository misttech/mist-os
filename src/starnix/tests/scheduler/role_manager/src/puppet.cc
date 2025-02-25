// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <sys/resource.h>
#include <unistd.h>

#include <filesystem>
#include <fstream>
#include <functional>
#include <iostream>
#include <string>
#include <thread>

using namespace std::chrono_literals;

namespace {

constexpr std::string kThreadMessage = "thread";
constexpr std::string kForkMessage = "fork";

void wait_for_control_message(const std::function<void(const std::string&)>& validator) {
  std::string control_message;
  std::getline(std::cin, control_message);
  validator(control_message);
}

void wait_for_expected_control_message(const std::string& expected) {
  std::cout << "waiting for `" << expected << "` control message...\n";
  wait_for_control_message([expected](const std::string& msg) {
    if (msg != expected) {
      std::cout << "expected `" << expected << "` control message, got `" << msg << "`\n";
      abort();
    }
  });
}

void set_priority_or_panic(int new_nice) {
  wait_for_control_message([new_nice](const std::string& msg) {
    int requested = std::atoi(msg.c_str());
    if (requested != new_nice) {
      std::cout << "test controller requested an unexpected nice. code says " << new_nice
                << ", socket says `" << requested << "`\n";
      abort();
    }
  });

  if (setpriority(PRIO_PROCESS, 0, new_nice)) {
    std::cout << "failed to update nice: " << std::strerror(errno) << "\n";
    abort();
  }
  std::cout << "set nice to " << new_nice << "\n";
}

void spawn_and_join_thread_with_nice(int child_nice) {
  wait_for_expected_control_message(kThreadMessage);
  std::thread child([child_nice]() { set_priority_or_panic(child_nice); });
  child.join();
}

}  // namespace

int main(int argc, const char** argv) {
  std::cout << "starting starnix puppet...\n";
  std::filesystem::path child_fence_path("/tmp/child.done");

  set_priority_or_panic(10);
  spawn_and_join_thread_with_nice(12);

  wait_for_expected_control_message(kForkMessage);
  std::cout << "forking child process...\n";
  // TODO(b/297961833) test SCHED_RESET_ON_FORK
  pid_t child = fork();
  if (child > 0) {
    // parent process waits for child process to finish
    while (true) {
      if (std::filesystem::exists(child_fence_path)) {
        break;
      }
      std::this_thread::sleep_for(5ms);
    }
    std::cout << "child reported done, exiting.";
  } else {
    // child process emits some scheduler calls and writes to its fence when done
    set_priority_or_panic(14);
    spawn_and_join_thread_with_nice(16);
    std::ofstream child_fence(child_fence_path);
    child_fence << "done!";
  }

  return 0;
}
