// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_COMPONENTS_PW_RPC_RUNNER_COMPONENT_RUNNER_H_
#define EXAMPLES_COMPONENTS_PW_RPC_RUNNER_COMPONENT_RUNNER_H_

#include <fidl/fuchsia.component.runner/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <fidl/fuchsia.logger/cpp/fidl.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/dispatcher.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fit/function.h>
#include <lib/zx/eventpair.h>
#include <lib/zx/result.h>

#include <memory>
#include <optional>
#include <string>
#include <unordered_map>

#include "examples/components/pw_rpc/runner/connection.h"
#include "examples/components/pw_rpc/runner/remote_endpoint.h"
#include "pw_stream/socket_stream.h"

class ComponentInstance : public fidl::Server<fuchsia_component_runner::ComponentController> {
 public:
  // Creates an object that manages a component instance's runtime.
  //
  // The object has ownership of the `ComponentController`, and implements the `ComponentController`
  // server.
  //
  // `stream` should contain a socket to the remote Pigweed server endpoint. The socket should have
  // the `O_NONBLOCK` flag.
  ComponentInstance(
      async_dispatcher_t* dispatcher, pw::stream::SocketStream stream,
      fidl::Client<fuchsia_logger::LogSink> log_sink,
      fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller_server_end,
      fit::callback<void()> deletion_cb);
  ~ComponentInstance() override;
  ComponentInstance(ComponentInstance&&) = delete;
  ComponentInstance& operator=(ComponentInstance&&) = delete;

  zx::result<> Serve(fidl::ServerEnd<fuchsia_io::Directory> outgoing_dir_server_end);

  void Stop(StopCompleter::Sync& completer) override;
  void Kill(KillCompleter::Sync& completer) override;

  void Start();

  void Close();

 private:
  zx::result<pw::stream::SocketStream> OpenLoggerStream();

  async_dispatcher_t* const dispatcher_;
  fidl::ServerBinding<fuchsia_component_runner::ComponentController> binding_;
  std::shared_ptr<ConnectionGroup> connections_;
  fidl::Client<fuchsia_logger::LogSink> log_sink_;
  component::OutgoingDirectory outgoing_dir_;
  fit::callback<void()> deletion_cb_;
  zx::eventpair disconnect_event_;
  std::optional<async::WaitOnce> disconnect_wait_;
};

class ComponentRunnerImpl : public fidl::Server<fuchsia_component_runner::ComponentRunner> {
 public:
  explicit ComponentRunnerImpl(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}
  void Start(StartRequest& request, StartCompleter::Sync& completer) override;

 private:
  zx::result<> DoStart(fuchsia_component_runner::ComponentStartInfo start_info,
                       fidl::ServerEnd<fuchsia_component_runner::ComponentController>& controller);

  async_dispatcher_t* const dispatcher_;
  std::unordered_map<uint64_t, std::unique_ptr<ComponentInstance>> components_;
  uint64_t next_component_id_ = 0;
};

#endif  // EXAMPLES_COMPONENTS_PW_RPC_RUNNER_COMPONENT_RUNNER_H_
