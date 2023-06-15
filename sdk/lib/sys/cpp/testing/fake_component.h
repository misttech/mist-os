// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SYS_CPP_TESTING_FAKE_COMPONENT_H_
#define LIB_SYS_CPP_TESTING_FAKE_COMPONENT_H_

#include <lib/async/dispatcher.h>
#include <lib/sys/cpp/testing/fake_launcher.h>

#include <memory>
#include <utility>

namespace vfs {
class PseudoDir;
}  // namespace vfs

namespace sys {
namespace testing {

// A fake component which can be used to intercept component launch using
// |FakeLauncher| and publish fake services for unit testing.
class FakeComponent {
 public:
  FakeComponent();
  ~FakeComponent();

  // Adds specified interface to the set of public interfaces.
  //
  // Adds a supported service with the given |service_name|, using the given
  // |interface_request_handler|, which should remain valid for the lifetime of
  // this object.
  //
  // A typical usage may be:
  //
  //   AddPublicService(foobar_bindings_.GetHandler(this));
  template <typename Interface>
  zx_status_t AddPublicService(fidl::InterfaceRequestHandler<Interface> handler,
                               std::string service_name = Interface::Name_) {
    return AddPublicService(
        [handler = std::move(handler)](zx::channel channel, async_dispatcher_t* dispatcher) {
          handler(fidl::InterfaceRequest<Interface>(std::move(channel)));
        },
        std::move(service_name));
  }

  // Registers this component with a FakeLauncher.
  void Register(std::string url, FakeLauncher& fake_launcher,
                async_dispatcher_t* dispatcher = nullptr);

 private:
  using Connector = fit::function<void(zx::channel channel, async_dispatcher_t* dispatcher)>;

  zx_status_t AddPublicService(Connector connector, std::string service_name);

  std::unique_ptr<vfs::PseudoDir> directory_;
  std::vector<fidl::InterfaceRequest<fuchsia::sys::ComponentController>> ctrls_;
};

}  // namespace testing
}  // namespace sys

#endif  // LIB_SYS_CPP_TESTING_FAKE_COMPONENT_H_
