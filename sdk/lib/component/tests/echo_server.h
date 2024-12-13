// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_COMPONENT_TESTS_ECHO_SERVER_H_
#define LIB_COMPONENT_TESTS_ECHO_SERVER_H_

#include <fidl/fidl.service.test/cpp/wire.h>
#include <fidl/fidl.service.test/cpp/wire_test_base.h>
#include <lib/syslog/cpp/macros.h>

using Echo = fidl_service_test::Echo;
using EchoService = fidl_service_test::EchoService;

class EchoCommon : public fidl::WireServer<Echo> {
 public:
  explicit EchoCommon(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  explicit EchoCommon(const char* prefix, async_dispatcher_t* dispatcher)
      : prefix_(prefix), dispatcher_(dispatcher) {}

  void Connect(fidl::ServerEnd<Echo> request) {
    bindings_.AddBinding(dispatcher_, std::move(request), this, fidl::kIgnoreBindingClosure);
  }

  void Clone(CloneRequestView request, CloneCompleter::Sync& completer) override {
    Connect(fidl::ServerEnd<Echo>(request->request.TakeChannel()));
  }

  void EchoString(EchoStringRequestView request, EchoStringCompleter::Sync& completer) override {
    std::string reply;
    if (!prefix_.empty()) {
      reply += prefix_ + ": ";
    }
    reply += std::string(request->value.data(), request->value.size());
    completer.Reply(fidl::StringView::FromExternal(reply));
  }

  fidl::ProtocolHandler<Echo> CreateHandler() {
    return bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure);
  }

 private:
  std::string prefix_;
  async_dispatcher_t* dispatcher_;
  fidl::ServerBindingGroup<Echo> bindings_;
};

#endif  // LIB_COMPONENT_TESTS_ECHO_SERVER_H_
