// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.serversuite/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include <iostream>

#include "src/tests/fidl/server_suite/cpp_util/error_util.h"

class ClosedTargetControllerServer : public fidl::Server<fidl_serversuite::ClosedTargetController> {
 public:
  ClosedTargetControllerServer() = default;

  void CloseWithEpitaph(CloseWithEpitaphRequest& request,
                        CloseWithEpitaphCompleter::Sync& completer) override {
    sut_binding_->Close(request.epitaph_status());
  }

  void set_sut_binding(fidl::ServerBindingRef<fidl_serversuite::ClosedTarget> sut_binding) {
    ZX_ASSERT_MSG(!sut_binding_, "sut binding already set");
    sut_binding_ = std::move(sut_binding);
  }

 private:
  std::optional<fidl::ServerBindingRef<fidl_serversuite::ClosedTarget>> sut_binding_;
};

class ClosedTargetServer : public fidl::Server<fidl_serversuite::ClosedTarget> {
 public:
  ClosedTargetServer() = default;

  void OneWayNoPayload(OneWayNoPayloadCompleter::Sync& completer) override {
    std::cout << "ClosedTarget.OneWayNoPayload()" << std::endl;
    auto result = fidl::SendEvent(controller_binding_.value())->ReceivedOneWayNoPayload();
    ZX_ASSERT(result.is_ok());
  }

  void TwoWayNoPayload(TwoWayNoPayloadCompleter::Sync& completer) override {
    std::cout << "ClosedTarget.TwoWayNoPayload()" << std::endl;
    completer.Reply();
  }

  void TwoWayStructPayload(TwoWayStructPayloadRequest& request,
                           TwoWayStructPayloadCompleter::Sync& completer) override {
    std::cout << "ClosedTarget.TwoWayStructPayload()" << std::endl;
    completer.Reply(request.v());
  }

  void TwoWayTablePayload(TwoWayTablePayloadRequest& request,
                          TwoWayTablePayloadCompleter::Sync& completer) override {
    std::cout << "ClosedTarget.TwoWayTablePayload()" << std::endl;
    fidl_serversuite::ClosedTargetTwoWayTablePayloadResponse response({.v = request.v()});
    completer.Reply(response);
  }

  void TwoWayUnionPayload(TwoWayUnionPayloadRequest& request,
                          TwoWayUnionPayloadCompleter::Sync& completer) override {
    std::cout << "ClosedTarget.TwoWayUnionPayload()" << std::endl;
    ZX_ASSERT(request.v().has_value());
    completer.Reply(
        fidl_serversuite::ClosedTargetTwoWayUnionPayloadResponse::WithV(request.v().value()));
  }

  void TwoWayResult(TwoWayResultRequest& request, TwoWayResultCompleter::Sync& completer) override {
    std::cout << "ClosedTarget.TwoWayResult()" << std::endl;
    switch (request.Which()) {
      case TwoWayResultRequest::Tag::kPayload:
        completer.Reply(fit::ok(request.payload().value()));
        return;
      case TwoWayResultRequest::Tag::kError:
        completer.Reply(fit::error(request.error().value()));
        return;
    }
    ZX_PANIC("unhandled case");
  }

  void GetHandleRights(GetHandleRightsRequest& request,
                       GetHandleRightsCompleter::Sync& completer) override {
    zx_info_handle_basic_t info;
    ZX_ASSERT(ZX_OK == request.handle().get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr,
                                                 nullptr));
    completer.Reply(info.rights);
  }

  void GetSignalableEventRights(GetSignalableEventRightsRequest& request,
                                GetSignalableEventRightsCompleter::Sync& completer) override {
    zx_info_handle_basic_t info;
    ZX_ASSERT(ZX_OK == request.handle().get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr,
                                                 nullptr));
    completer.Reply(info.rights);
  }

  void EchoAsTransferableSignalableEvent(
      EchoAsTransferableSignalableEventRequest& request,
      EchoAsTransferableSignalableEventCompleter::Sync& completer) override {
    completer.Reply(zx::event(request.handle().release()));
  }

  void ByteVectorSize(ByteVectorSizeRequest& request,
                      ByteVectorSizeCompleter::Sync& completer) override {
    completer.Reply(static_cast<uint32_t>(request.vec().size()));
  }

  void HandleVectorSize(HandleVectorSizeRequest& request,
                        HandleVectorSizeCompleter::Sync& completer) override {
    completer.Reply(static_cast<uint32_t>(request.vec().size()));
  }

  void CreateNByteVector(CreateNByteVectorRequest& request,
                         CreateNByteVectorCompleter::Sync& completer) override {
    std::vector<uint8_t> bytes(request.n());
    completer.Reply(std::move(bytes));
  }

  void CreateNHandleVector(CreateNHandleVectorRequest& request,
                           CreateNHandleVectorCompleter::Sync& completer) override {
    std::vector<zx::event> handles(request.n());
    for (auto& handle : handles) {
      ZX_ASSERT(ZX_OK == zx::event::create(0, &handle));
    }
    completer.Reply(std::move(handles));
  }

  void OnUnbound(fidl::UnbindInfo info, fidl::ServerEnd<fidl_serversuite::ClosedTarget>) {
    if (!info.is_dispatcher_shutdown() && !info.is_user_initiated() && !info.is_peer_closed()) {
      std::cout << "ClosedTarget unbound with error: " << info.FormatDescription() << std::endl;
    }
    (void)fidl::SendEvent(controller_binding_.value())
        ->WillTeardown({{.reason = servertest_util::ClassifyTeardownReason(info)}});
  }

  void set_controller_binding(
      fidl::ServerBindingRef<fidl_serversuite::ClosedTargetController> controller_binding) {
    ZX_ASSERT_MSG(!controller_binding_, "controller binding already set");
    controller_binding_ = std::move(controller_binding);
  }

 private:
  std::optional<fidl::ServerBindingRef<fidl_serversuite::ClosedTargetController>>
      controller_binding_;
};

class AjarTargetServer : public fidl::Server<fidl_serversuite::AjarTarget> {
 public:
  explicit AjarTargetServer(fidl::ServerEnd<fidl_serversuite::AjarTargetController> controller)
      : controller_(std::move(controller)) {}

  void handle_unknown_method(fidl::UnknownMethodMetadata<fidl_serversuite::AjarTarget> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    auto result = fidl::SendEvent(controller_)
                      ->ReceivedUnknownMethod(fidl_serversuite::UnknownMethodInfo(
                          metadata.method_ordinal, fidl_serversuite::UnknownMethodType::kOneWay));
    ZX_ASSERT(result.is_ok());
  }

  void OnUnbound(fidl::UnbindInfo info, fidl::ServerEnd<fidl_serversuite::AjarTarget>) {
    if (!info.is_dispatcher_shutdown() && !info.is_user_initiated() && !info.is_peer_closed()) {
      std::cout << "AjarTarget unbound with error: " << info.FormatDescription() << std::endl;
    }
    (void)fidl::SendEvent(controller_)
        ->WillTeardown({{.reason = servertest_util::ClassifyTeardownReason(info)}});
  }

 private:
  fidl::ServerEnd<fidl_serversuite::AjarTargetController> controller_;
};

class OpenTargetControllerServer : public fidl::Server<fidl_serversuite::OpenTargetController> {
 public:
  OpenTargetControllerServer() = default;

  void SendStrictEvent(SendStrictEventCompleter::Sync& completer) override {
    auto result = fidl::SendEvent(sut_binding_.value())->StrictEvent();
    completer.Reply(result.map_error(
        [](auto error) { return servertest_util::ClassifySendEventError(error); }));
  }

  void SendFlexibleEvent(SendFlexibleEventCompleter::Sync& completer) override {
    auto result = fidl::SendEvent(sut_binding_.value())->FlexibleEvent();
    completer.Reply(result.map_error(
        [](auto error) { return servertest_util::ClassifySendEventError(error); }));
  }

  void set_sut_binding(fidl::ServerBindingRef<fidl_serversuite::OpenTarget> sut_binding) {
    ZX_ASSERT_MSG(!sut_binding_, "sut binding already set");
    sut_binding_ = std::move(sut_binding);
  }

 private:
  std::optional<fidl::ServerBindingRef<fidl_serversuite::OpenTarget>> sut_binding_;
};

class OpenTargetServer : public fidl::Server<fidl_serversuite::OpenTarget> {
 public:
  OpenTargetServer() = default;

  void StrictOneWay(StrictOneWayCompleter::Sync& completer) override {
    auto result = fidl::SendEvent(controller_binding_.value())->ReceivedStrictOneWay();
    ZX_ASSERT(result.is_ok());
  }

  void FlexibleOneWay(FlexibleOneWayCompleter::Sync& completer) override {
    auto result = fidl::SendEvent(controller_binding_.value())->ReceivedFlexibleOneWay();
    ZX_ASSERT(result.is_ok());
  }

  void StrictTwoWay(StrictTwoWayCompleter::Sync& completer) override { completer.Reply(); }

  void StrictTwoWayFields(StrictTwoWayFieldsRequest& request,
                          StrictTwoWayFieldsCompleter::Sync& completer) override {
    completer.Reply(request.reply_with());
  }

  void StrictTwoWayErr(StrictTwoWayErrRequest& request,
                       StrictTwoWayErrCompleter::Sync& completer) override {
    switch (request.Which()) {
      case fidl_serversuite::OpenTargetStrictTwoWayErrRequest::Tag::kReplySuccess:
        completer.Reply(fit::ok());
        break;
      case fidl_serversuite::OpenTargetStrictTwoWayErrRequest::Tag::kReplyError:
        completer.Reply(fit::error(request.reply_error().value()));
        break;
    }
  }

  void StrictTwoWayFieldsErr(StrictTwoWayFieldsErrRequest& request,
                             StrictTwoWayFieldsErrCompleter::Sync& completer) override {
    switch (request.Which()) {
      case fidl_serversuite::OpenTargetStrictTwoWayFieldsErrRequest::Tag::kReplySuccess:
        completer.Reply(fit::ok(request.reply_success().value()));
        break;
      case fidl_serversuite::OpenTargetStrictTwoWayFieldsErrRequest::Tag::kReplyError:
        completer.Reply(fit::error(request.reply_error().value()));
        break;
    }
  }

  void FlexibleTwoWay(FlexibleTwoWayCompleter::Sync& completer) override { completer.Reply(); }

  void FlexibleTwoWayFields(FlexibleTwoWayFieldsRequest& request,
                            FlexibleTwoWayFieldsCompleter::Sync& completer) override {
    completer.Reply(request.reply_with());
  }

  void FlexibleTwoWayErr(FlexibleTwoWayErrRequest& request,
                         FlexibleTwoWayErrCompleter::Sync& completer) override {
    switch (request.Which()) {
      case fidl_serversuite::OpenTargetFlexibleTwoWayErrRequest::Tag::kReplySuccess:
        completer.Reply(fit::ok());
        break;
      case fidl_serversuite::OpenTargetFlexibleTwoWayErrRequest::Tag::kReplyError:
        completer.Reply(fit::error(request.reply_error().value()));
        break;
    }
  }

  void FlexibleTwoWayFieldsErr(FlexibleTwoWayFieldsErrRequest& request,
                               FlexibleTwoWayFieldsErrCompleter::Sync& completer) override {
    switch (request.Which()) {
      case fidl_serversuite::OpenTargetFlexibleTwoWayFieldsErrRequest::Tag::kReplySuccess:
        completer.Reply(fit::ok(request.reply_success().value()));
        break;
      case fidl_serversuite::OpenTargetFlexibleTwoWayFieldsErrRequest::Tag::kReplyError:
        completer.Reply(fit::error(request.reply_error().value()));
        break;
    }
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fidl_serversuite::OpenTarget> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    fidl_serversuite::UnknownMethodType method_type;
    switch (metadata.unknown_method_type) {
      case fidl::UnknownMethodType::kOneWay:
        method_type = fidl_serversuite::UnknownMethodType::kOneWay;
        break;
      case fidl::UnknownMethodType::kTwoWay:
        method_type = fidl_serversuite::UnknownMethodType::kTwoWay;
        break;
    }
    auto result = fidl::SendEvent(controller_binding_.value())
                      ->ReceivedUnknownMethod(fidl_serversuite::UnknownMethodInfo(
                          metadata.method_ordinal, method_type));
    ZX_ASSERT(result.is_ok());
  }

  void OnUnbound(fidl::UnbindInfo info, fidl::ServerEnd<fidl_serversuite::OpenTarget>) {
    if (!info.is_dispatcher_shutdown() && !info.is_user_initiated() && !info.is_peer_closed()) {
      std::cout << "OpenTarget unbound with error: " << info.FormatDescription() << std::endl;
    }
    (void)fidl::SendEvent(controller_binding_.value())
        ->WillTeardown({{.reason = servertest_util::ClassifyTeardownReason(info)}});
  }

  void set_controller_binding(
      fidl::ServerBindingRef<fidl_serversuite::OpenTargetController> controller_binding) {
    ZX_ASSERT_MSG(!controller_binding_, "controller binding already set");
    controller_binding_ = std::move(controller_binding);
  }

 private:
  std::optional<fidl::ServerBindingRef<fidl_serversuite::OpenTargetController>> controller_binding_;
};

class RunnerServer : public fidl::Server<fidl_serversuite::Runner> {
 public:
  explicit RunnerServer(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  void IsTestEnabled(IsTestEnabledRequest& request,
                     IsTestEnabledCompleter::Sync& completer) override {
    bool is_enabled = [request]() {
      switch (request.test()) {
        case fidl_serversuite::Test::kIgnoreDisabled:
          // This case will forever be false, as it is intended to validate the "test disabling"
          // functionality of the runner itself.
          return false;

        case fidl_serversuite::Test::kOneWayWithNonZeroTxid:
        case fidl_serversuite::Test::kTwoWayNoPayloadWithZeroTxid:
          return false;

        default:
          return true;
      }
    }();
    completer.Reply(is_enabled);
  }

  void IsTeardownReasonSupported(IsTeardownReasonSupportedCompleter::Sync& completer) override {
    completer.Reply({{.is_supported = true}});
  }

  void Start(StartRequest& request, StartCompleter::Sync& completer) override {
    std::cout << "Runner.Start()" << std::endl;

    switch (request.target().Which()) {
      case fidl_serversuite::AnyTarget::Tag::kClosedTarget: {
        auto& server_pair = request.target().closed_target().value();
        auto controller_server = std::make_shared<ClosedTargetControllerServer>();
        auto sut_server = std::make_shared<ClosedTargetServer>();

        auto controller_binding =
            fidl::BindServer(dispatcher_, std::move(server_pair.controller()), controller_server,
                             [](auto*, fidl::UnbindInfo info, auto) {
                               if (!info.is_dispatcher_shutdown() && !info.is_user_initiated() &&
                                   !info.is_peer_closed()) {
                                 std::cerr << "ClosedTargetController unbound with error: "
                                           << info.FormatDescription() << std::endl;
                               }
                             });
        auto sut_binding = fidl::BindServer(dispatcher_, std::move(server_pair.sut()), sut_server,
                                            std::mem_fn(&ClosedTargetServer::OnUnbound));

        // This is thread safe because the new server runs in the same
        // dispatcher thread as the request to start it.
        controller_server->set_sut_binding(sut_binding);
        sut_server->set_controller_binding(controller_binding);

        completer.Reply();
        break;
      }
      case fidl_serversuite::AnyTarget::Tag::kAjarTarget: {
        auto& server_pair = request.target().ajar_target().value();
        auto sut_server = std::make_unique<AjarTargetServer>(std::move(server_pair.controller()));
        fidl::BindServer(dispatcher_, std::move(server_pair.sut()), std::move(sut_server),
                         std::mem_fn(&AjarTargetServer::OnUnbound));
        completer.Reply();
        break;
      }
      case fidl_serversuite::AnyTarget::Tag::kOpenTarget: {
        auto& server_pair = request.target().open_target().value();
        auto controller_server = std::make_shared<OpenTargetControllerServer>();
        auto sut_server = std::make_shared<OpenTargetServer>();

        auto controller_binding = fidl::BindServer(
            dispatcher_, std::move(server_pair.controller()), controller_server,
            [](auto*, fidl::UnbindInfo info, auto) {
              if (!info.is_dispatcher_shutdown() && !info.is_user_initiated() &&
                  !info.is_peer_closed()) {
                std::cerr << "OpenTargetController unbound with error: " << info.FormatDescription()
                          << std::endl;
              }
            });
        auto sut_binding = fidl::BindServer(dispatcher_, std::move(server_pair.sut()), sut_server,
                                            std::mem_fn(&OpenTargetServer::OnUnbound));

        // This is thread safe because the new server runs in the same
        // dispatcher thread as the request to start it.
        controller_server->set_sut_binding(sut_binding);
        sut_server->set_controller_binding(controller_binding);
        completer.Reply();
        break;
      }
    }
  }

  void CheckAlive(CheckAliveCompleter::Sync& completer) override { completer.Reply(); }

 private:
  async_dispatcher_t* dispatcher_;
};

int main(int argc, const char** argv) {
  std::cout << "CPP server: main" << std::endl;
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  auto outgoing = component::OutgoingDirectory(loop.dispatcher());
  ZX_ASSERT(outgoing.ServeFromStartupInfo().is_ok());
  RunnerServer runner_server(loop.dispatcher());
  auto result = outgoing.AddProtocol<fidl_serversuite::Runner>(
      std::make_unique<RunnerServer>(loop.dispatcher()));
  ZX_ASSERT(result.is_ok());

  std::cout << "CPP server: ready!" << std::endl;
  return loop.Run();
}
