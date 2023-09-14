// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/components/pw_rpc/runner/component_runner.h"

#include <fcntl.h>
#include <fidl/fuchsia.component.runner/cpp/fidl.h>
#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.data/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <fidl/fuchsia.logger/cpp/fidl.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/dispatcher.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdio/directory.h>
#include <lib/fit/function.h>
#include <lib/syslog/cpp/log_level.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/eventpair.h>
#include <lib/zx/result.h>
#include <lib/zx/socket.h>
#include <zircon/status.h>

#include <cstdlib>
#include <limits>
#include <memory>
#include <optional>
#include <string>

#include "examples/components/pw_rpc/runner/connection.h"
#include "examples/components/pw_rpc/runner/log_proxy.h"
#include "examples/components/pw_rpc/runner/multiplexer.h"
#include "pw_stream/socket_stream.h"

ComponentInstance::ComponentInstance(
    async_dispatcher_t* dispatcher, pw::stream::SocketStream stream,
    fidl::Client<fuchsia_logger::LogSink> log_sink,
    fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller_server_end,
    fit::callback<void()> deletion_cb)
    : dispatcher_(dispatcher),
      binding_(dispatcher_, std::move(controller_server_end), this, fidl::kIgnoreBindingClosure),
      connections_(std::make_shared<ConnectionGroup>(std::move(stream))),
      log_sink_(std::move(log_sink)),
      outgoing_dir_(dispatcher_),
      deletion_cb_(std::move(deletion_cb)) {}

ComponentInstance::~ComponentInstance() { binding_.Close(ZX_OK); }

zx::result<> ComponentInstance::Serve(
    fidl::ServerEnd<fuchsia_io::Directory> outgoing_dir_server_end) {
  std::unique_ptr remote_endpoint_impl = std::make_unique<RemoteEndpoint>(connections_);
  if (auto result = outgoing_dir_.AddProtocol<fidl_examples_pigweed::RemoteEndpoint>(
          std::move(remote_endpoint_impl));
      result.is_error()) {
    return result;
  }
  return outgoing_dir_.Serve(std::move(outgoing_dir_server_end));
}

void ComponentInstance::Start() {
  zx::socket log_socket, log_server_socket;
  zx::socket::create(0, &log_socket, &log_server_socket);
  if (!log_socket.is_valid()) {
    FX_SLOG(WARNING, "Failed to establish connection to log sink. Terminating component.");
    Close();
    return;
  }
  if (auto result = log_sink_->ConnectStructured(std::move(log_server_socket)); result.is_error()) {
    FX_SLOG(WARNING, "Failed to establish connection to log sink. Terminating component.");
    Close();
    return;
  }
  auto result = OpenLoggerStream();
  if (result.is_error()) {
    FX_SLOG(WARNING, "Failed to open connection for log forwarding. Terminating component.",
            KV("status", result.status_string()));
    Close();
    return;
  }
  pw::stream::SocketStream logger_stream = std::move(*result);
  zx::eventpair other;
  FX_CHECK(zx::eventpair::create(0, &disconnect_event_, &other) == ZX_OK);
  disconnect_wait_.emplace(disconnect_event_.get(), ZX_EVENTPAIR_PEER_CLOSED, 0u);
  disconnect_wait_->Begin(
      dispatcher_, [this](async_dispatcher_t* dispatcher, async::WaitOnce* wait, zx_status_t status,
                          const zx_packet_signal_t* signal) {
        FX_CHECK(status == ZX_OK);
        Close();
      });
  LogProxy log_proxy(std::move(logger_stream), std::move(log_socket));
  log_proxy.Detach();

  Multiplexer multiplexer(connections_, std::move(other));
  multiplexer.Detach();
}

void ComponentInstance::Stop(StopCompleter::Sync& completer) { Close(); }

void ComponentInstance::Kill(KillCompleter::Sync& completer) { Close(); }

void ComponentInstance::Close() {
  // This will end up invoking the destructor, which unbinds the channel.
  deletion_cb_();
}

zx::result<pw::stream::SocketStream> ComponentInstance::OpenLoggerStream() {
  auto result = OpenConnection(connections_);
  if (result.is_error()) {
    return result.take_error();
  }
  zx::socket logger_sock = std::move(*result);
  int connection_fd;
  if (auto status = fdio_fd_create(logger_sock.release(), &connection_fd); status != ZX_OK) {
    return zx::error(status);
  }

  return zx::success(pw::stream::SocketStream(connection_fd));
}

void ComponentRunnerImpl::Start(StartRequest& request, StartCompleter::Sync& completer) {
  auto& controller = request.controller();
  if (auto result = DoStart(std::move(request.start_info()), controller); result.is_error()) {
    controller.Close(result.error_value());
  }
}

constexpr zx_status_t InstanceCannotStart() {
  return static_cast<zx_status_t>(fuchsia_component::Error::kInstanceCannotStart);
}

static zx::result<std::tuple<std::string, uint16_t>> ExtractHostPort(
    const fuchsia_component_runner::ComponentStartInfo& start_info) {
  std::optional<std::string> host;
  std::optional<uint16_t> port;
  if (!start_info.program().has_value() || !start_info.program()->entries().has_value()) {
    FX_SLOG(WARNING, "Attempted to launch component without program.");
    return zx::error(InstanceCannotStart());
  }
  for (const fuchsia_data::DictionaryEntry& entry : *start_info.program()->entries()) {
    if (entry.key() == "host") {
      if (entry.value()->Which() != fuchsia_data::DictionaryValue::Tag::kStr) {
        FX_SLOG(WARNING, "Attempted to launch component with malformed host.");
        return zx::error(InstanceCannotStart());
      }
      if (host.has_value()) {
        FX_SLOG(WARNING, "Attempted to launch component with duplicate host.");
        return zx::error(InstanceCannotStart());
      }
      host.emplace(entry.value()->str().value());
    } else if (entry.key() == "port") {
      if (entry.value()->Which() != fuchsia_data::DictionaryValue::Tag::kStr) {
        FX_SLOG(WARNING, "Attempted to launch component with malformed port.");
        return zx::error(InstanceCannotStart());
      }
      if (port.has_value()) {
        FX_SLOG(WARNING, "Attempted to launch component with duplicate port.");
        return zx::error(InstanceCannotStart());
      }
      char* end = nullptr;
      const unsigned long port_long = std::strtoul(entry.value()->str().value().c_str(), &end, 10);
      if (errno == ERANGE || port_long > std::numeric_limits<uint16_t>::max()) {
        FX_SLOG(WARNING, "Attempted to launch component with malformed port.");
        return zx::error(InstanceCannotStart());
      }
      port.emplace(static_cast<uint16_t>(port_long));
    } else {
      FX_SLOG(WARNING, "Attempted to launch component with unrecognized property.");
      return zx::error(InstanceCannotStart());
    }
  }
  if (!host.has_value() || !port.has_value()) {
    FX_SLOG(WARNING, "Attempted to launch component without host or port.");
    return zx::error(InstanceCannotStart());
  }
  return zx::success(std::make_tuple(*host, *port));
}

static fidl::Client<fuchsia_logger::LogSink> ConnectLogSink(
    async_dispatcher_t* dispatcher, fuchsia_component_runner::ComponentStartInfo& start_info) {
  if (start_info.ns().has_value()) {
    for (const fuchsia_component_runner::ComponentNamespaceEntry& entry : *start_info.ns()) {
      if (entry.path() == "/svc") {
        if (!entry.directory().has_value()) {
          continue;
        }
        auto log_sink_endpoints = fidl::CreateEndpoints<fuchsia_logger::LogSink>();
        FX_CHECK(log_sink_endpoints.is_ok());
        auto [log_sink_client_end, log_sink_server_end] = *std::move(log_sink_endpoints);
        const zx_handle_t svc_handle = entry.directory()->channel().get();
        if (zx_status_t status = fdio_service_connect_at(
                svc_handle, "fuchsia.logger.LogSink", log_sink_server_end.TakeChannel().release());
            status != ZX_OK) {
          FX_SLOG(WARNING, "Failed to open component's /svc.",
                  KV("status", zx_status_get_string(status)));
        }
        return fidl::Client<fuchsia_logger::LogSink>(std::move(log_sink_client_end), dispatcher);
      }
    }
  }
  return fidl::Client<fuchsia_logger::LogSink>{};
}

zx::result<> ComponentRunnerImpl::DoStart(
    fuchsia_component_runner::ComponentStartInfo start_info,
    fidl::ServerEnd<fuchsia_component_runner::ComponentController>& controller) {
  auto result = ExtractHostPort(start_info);
  if (result.is_error()) {
    return result.take_error();
  }
  const auto [host, port] = std::move(*result);

  if (!start_info.outgoing_dir().has_value()) {
    FX_SLOG(WARNING, "Attempted to launch component without outgoing dir.");
    return zx::error(InstanceCannotStart());
  }

  // TODO: This is blocking, make it async or spin up another thread.
  pw::stream::SocketStream stream;
  if (!stream.Connect(host.c_str(), port).ok()) {
    FX_SLOG(WARNING, "Failed to connect to remote endpoint", KV("host", host), KV("port", port));
    return zx::error(InstanceCannotStart());
  }
  const int flags = fcntl(stream.connection_fd(), F_GETFD, 0) | O_NONBLOCK;
  FX_CHECK(fcntl(stream.connection_fd(), F_SETFD, flags) == 0);

  fidl::Client<fuchsia_logger::LogSink> log_sink = ConnectLogSink(dispatcher_, start_info);
  const uint64_t component_id = next_component_id_++;
  auto deletion_cb = [this, component_id]() { components_.erase(component_id); };
  std::unique_ptr instance =
      std::make_unique<ComponentInstance>(dispatcher_, std::move(stream), std::move(log_sink),
                                          std::move(controller), std::move(deletion_cb));
  if (auto result = instance->Serve(std::move(*start_info.outgoing_dir())); result.is_error()) {
    FX_SLOG(WARNING, "Failed to serve component's outgoing dir.",
            KV("status", result.status_string()));
    return zx::error(InstanceCannotStart());
  }
  components_.emplace(component_id, std::move(instance));
  components_.at(component_id)->Start();

  return zx::ok();
}
