// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TESTING_TEST_PROCESSARGS_H_
#define LIB_LD_TESTING_TEST_PROCESSARGS_H_

#include <lib/zx/channel.h>
#include <lib/zx/handle.h>

#include <string>
#include <vector>

namespace ld::testing {

// This is a "builder"-style object that collects information to be packed into
// the processargs message(s).
class TestProcessArgs {
 public:
  TestProcessArgs& AddHandle(uint32_t info, zx::handle handle);

  // This duplicates the handle.
  TestProcessArgs& AddDuplicateHandle(uint32_t info, zx::unowned_handle ref);

  // This is just a shorthand for using PA_HND(PA_FD, fd).
  TestProcessArgs& AddFd(int fd, zx::handle handle);

  // These work with any zx::foo type.
  template <typename T>
  TestProcessArgs& AddHandle(uint32_t info, T&& handle) {
    return AddHandle(info, zx::handle{handle.release()});
  }

  template <typename T>
  TestProcessArgs& AddDuplicateHandle(uint32_t info, zx::unowned<T> ref) {
    return AddDuplicateHandle(info, zx::unowned_handle{ref->get()});
  }

  template <typename T>
  TestProcessArgs& AddFd(int fd, T&& handle) {
    return AddFd(fd, zx::handle{handle.release()});
  }

  // This returns the index of the entry the next AddName call will append.
  size_t next_name() const { return names_.size(); }

  // Append one name to the name table.
  TestProcessArgs& AddName(std::string_view name) {
    names_.emplace_back(name);
    return *this;
  }

  // This adds a name table entry along with a handle of the given PA_NS_* type
  // using the new entry's index.
  TestProcessArgs& AddName(std::string_view name, uint32_t info, zx::channel handle);

  // This replaces the entire name table.
  TestProcessArgs& SetNames(std::initializer_list<std::string_view> names) {
    names_ = std::vector<std::string>{names.begin(), names.end()};
    return *this;
  }

  TestProcessArgs& SetArgs(std::initializer_list<std::string_view> args) {
    args_ = std::vector<std::string>{args.begin(), args.end()};
    return *this;
  }

  TestProcessArgs& SetEnv(std::initializer_list<std::string_view> env) {
    env_ = std::vector<std::string>{env.begin(), env.end()};
    return *this;
  }

  // This calls AddHandle with the process, thread, etc.
  TestProcessArgs& AddInProcessTestHandles();

  // Add a fuchsia.ldsvc.Loader channel handle.  If this is called with a null
  // handle, it will clone the calling process's own loader service.
  TestProcessArgs& AddLdsvc(zx::channel ldsvc = {});

  // This packs up a message and sends it on the given channel.  When this
  // returns without raising a gtest fatal failure, the state of the object is
  // cleared out so it it could be used for another call.
  void PackBootstrap(zx::unowned_channel bootstrap_sender);

  // This creates a new channel and returns the receiver end to be passed to
  // the new process.  Before returning, it sends a bootstrap message on the
  // sender side of the channel and clears the builder state.  The sender side
  // of the channel is retained in this object.  If this is called again when a
  // channel already exists, it sends a second message on that same channel; in
  // that case it always returns a null handle.  If anything went wrong, this
  // raises a gtest fatal failure.
  zx::channel PackBootstrap();

  // Access the sender channel if one was created by PackBootstrap
  zx::channel& bootstrap_sender() { return bootstrap_sender_; }

 private:
  std::vector<zx::handle> handles_;
  std::vector<uint32_t> handle_info_;
  std::vector<std::string> args_, env_, names_;
  zx::channel bootstrap_sender_;
};

}  // namespace ld::testing

#endif  // LIB_LD_TESTING_TEST_PROCESSARGS_H_
