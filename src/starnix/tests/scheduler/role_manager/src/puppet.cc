// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <sys/resource.h>
#include <unistd.h>

#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>
#include <thread>

using namespace std::chrono_literals;

void set_priority_or_panic(int new_nice) {
  std::cout << "waiting for control message with " << new_nice << "...\n";
  std::string control_message;
  std::getline(std::cin, control_message);

  int requested = std::atoi(control_message.c_str());
  if (requested != new_nice) {
    std::cout << "test controller requested an unexpected nice. code says " << new_nice
              << ", socket says `" << control_message << "`\n";
    abort();
  }

  if (setpriority(PRIO_PROCESS, 0, new_nice)) {
    std::cout << "failed to update nice: " << std::strerror(errno) << "\n";
    abort();
  }
  std::cout << "set nice to " << new_nice << "\n";
}

void spawn_and_join_thread_with_nice(int child_nice) {
  std::cout << "waiting for control message to spawn thread...\n";
  std::string control_message;
  std::getline(std::cin, control_message);
  if (control_message != "thread") {
    std::cout << "expected new thread control message, got `" << control_message << "`\n";
    abort();
  }
  std::thread child([child_nice]() { set_priority_or_panic(child_nice); });
  child.join();
}

int main(int argc, const char** argv) {
  std::cout << "starting starnix puppet...\n";
  std::filesystem::path child_fence_path("/tmp/child.done");

  set_priority_or_panic(10);
  spawn_and_join_thread_with_nice(12);

  std::cout << "waiting for control message to fork child process...\n";
  std::string control_message;
  std::getline(std::cin, control_message);
  if (control_message != "fork") {
    std::cout << "expected fork control message, got `" << control_message << "`\n";
    abort();
  }

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
}
