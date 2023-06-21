// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/serversuite/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/synchronous_interface_ptr.h>
#include <lib/sys/cpp/component_context.h>
#include <stdio.h>

#include <iostream>
#include <memory>

#include "lib/fidl/cpp/interface_request.h"

template <fidl::serversuite::AnyTarget::Tag TARGET>
struct TargetTypes;

class ClosedTargetControllerServer : public fidl::serversuite::ClosedTargetController {
 public:
  ClosedTargetControllerServer() = default;

  void CloseWithEpitaph(int32_t epitaph_status) override {
    ZX_ASSERT(ZX_OK == sut_binding_->Close(epitaph_status));
  }

  void set_sut_binding(fidl::Binding<fidl::serversuite::ClosedTarget>* sut_binding) {
    sut_binding_ = sut_binding;
  }

 private:
  fidl::Binding<fidl::serversuite::ClosedTarget>* sut_binding_ = nullptr;
};

class ClosedTargetServer : public fidl::serversuite::ClosedTarget {
 public:
  ClosedTargetServer() = default;

  void OneWayNoPayload() override {
    std::cout << "ClosedTarget.OneWayNoPayload()" << std::endl;
    controller_binding_->events().ReceivedOneWayNoPayload();
  }

  void TwoWayNoPayload(TwoWayNoPayloadCallback callback) override {
    std::cout << "ClosedTarget.TwoWayNoPayload()" << std::endl;
    callback();
  }

  void TwoWayStructPayload(int8_t v, TwoWayStructPayloadCallback callback) override {
    std::cout << "ClosedTarget.TwoWayStructPayload()" << std::endl;
    callback(v);
  }

  void TwoWayTablePayload(::fidl::serversuite::ClosedTargetTwoWayTablePayloadRequest request,
                          TwoWayTablePayloadCallback callback) override {
    std::cout << "ClosedTarget.TwoWayTablePayload()" << std::endl;
    fidl::serversuite::ClosedTargetTwoWayTablePayloadResponse response;
    response.set_v(request.v());
    callback(std::move(response));
  }

  void TwoWayUnionPayload(::fidl::serversuite::ClosedTargetTwoWayUnionPayloadRequest request,
                          TwoWayUnionPayloadCallback callback) override {
    std::cout << "ClosedTarget.TwoWayUnionPayload()" << std::endl;
    fidl::serversuite::ClosedTargetTwoWayUnionPayloadResponse response;
    response.set_v(request.v());
    callback(std::move(response));
  }

  void TwoWayResult(::fidl::serversuite::ClosedTargetTwoWayResultRequest request,
                    TwoWayResultCallback callback) override {
    std::cout << "ClosedTarget.TwoWayResult()" << std::endl;
    switch (request.Which()) {
      case fidl::serversuite::ClosedTargetTwoWayResultRequest::kPayload:
        callback(fidl::serversuite::ClosedTarget_TwoWayResult_Result::WithResponse(
            fidl::serversuite::ClosedTarget_TwoWayResult_Response(request.payload())));
        break;
      case fidl::serversuite::ClosedTargetTwoWayResultRequest::kError:
        callback(fidl::serversuite::ClosedTarget_TwoWayResult_Result::WithErr(
            std::move(request.error())));
        break;
      case fidl::serversuite::ClosedTargetTwoWayResultRequest::Invalid:
        ZX_PANIC("unexpected invalid case");
        break;
    }
  }

  void GetHandleRights(zx::handle handle, GetHandleRightsCallback callback) override {
    zx_info_handle_basic_t info;
    ZX_ASSERT(ZX_OK ==
              handle.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
    callback(info.rights);
  }

  void GetSignalableEventRights(zx::event event,
                                GetSignalableEventRightsCallback callback) override {
    zx_info_handle_basic_t info;
    ZX_ASSERT(ZX_OK == event.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
    callback(info.rights);
  }

  void EchoAsTransferableSignalableEvent(
      zx::handle handle, EchoAsTransferableSignalableEventCallback callback) override {
    callback(zx::event(handle.release()));
  }

  void ByteVectorSize(std::vector<uint8_t> vec, ByteVectorSizeCallback callback) override {
    callback(static_cast<uint32_t>(vec.size()));
  }

  void HandleVectorSize(std::vector<zx::event> vec, HandleVectorSizeCallback callback) override {
    callback(static_cast<uint32_t>(vec.size()));
  }

  void CreateNByteVector(uint32_t n, CreateNByteVectorCallback callback) override {
    std::vector<uint8_t> bytes(n);
    callback(std::move(bytes));
  }

  void CreateNHandleVector(uint32_t n, CreateNHandleVectorCallback callback) override {
    std::vector<zx::event> handles(n);
    for (auto& handle : handles) {
      ZX_ASSERT(ZX_OK == zx::event::create(0, &handle));
    }
    callback(std::move(handles));
  }

  void set_controller_binding(
      fidl::Binding<fidl::serversuite::ClosedTargetController>* controller_binding) {
    controller_binding_ = controller_binding;
  }

 private:
  fidl::Binding<fidl::serversuite::ClosedTargetController>* controller_binding_ = nullptr;
};

template <>
struct TargetTypes<fidl::serversuite::AnyTarget::Tag::kClosedTarget> {
  using Controller = fidl::serversuite::ClosedTargetController;
  using Sut = fidl::serversuite::ClosedTarget;

  using ControllerServer = ClosedTargetControllerServer;
  using SutServer = ClosedTargetServer;

  using ServerPair = fidl::serversuite::ClosedTargetServerPair;
};

class AjarTargetControllerServer : public fidl::serversuite::AjarTargetController {
 public:
  AjarTargetControllerServer() = default;
};

class AjarTargetServer : public fidl::serversuite::AjarTarget {
 public:
  AjarTargetServer() = default;

  void set_controller_binding(
      fidl::Binding<fidl::serversuite::AjarTargetController>* controller_binding) {
    controller_binding_ = controller_binding;
  }

 protected:
  void handle_unknown_method(uint64_t ordinal) override {
    controller_binding_->events().ReceivedUnknownMethod(
        ordinal, fidl::serversuite::UnknownMethodType::ONE_WAY);
  }

 private:
  fidl::Binding<fidl::serversuite::AjarTargetController>* controller_binding_ = nullptr;
};

template <>
struct TargetTypes<fidl::serversuite::AnyTarget::Tag::kAjarTarget> {
  using Controller = fidl::serversuite::AjarTargetController;
  using Sut = fidl::serversuite::AjarTarget;

  using ControllerServer = AjarTargetControllerServer;
  using SutServer = AjarTargetServer;

  using ServerPair = fidl::serversuite::AjarTargetServerPair;
};

class OpenTargetControllerServer : public fidl::serversuite::OpenTargetController {
 public:
  OpenTargetControllerServer() = default;

  void SendStrictEvent(SendStrictEventCallback callback) override {
    sut_binding_->events().StrictEvent();
    callback(fidl::serversuite::OpenTargetController_SendStrictEvent_Result::WithResponse({}));
  }

  void SendFlexibleEvent(SendFlexibleEventCallback callback) override {
    sut_binding_->events().FlexibleEvent();
    callback(fidl::serversuite::OpenTargetController_SendFlexibleEvent_Result::WithResponse({}));
  }

  void set_sut_binding(fidl::Binding<fidl::serversuite::OpenTarget>* sut_binding) {
    sut_binding_ = sut_binding;
  }

 private:
  fidl::Binding<fidl::serversuite::OpenTarget>* sut_binding_ = nullptr;
};

class OpenTargetServer : public fidl::serversuite::OpenTarget {
 public:
  OpenTargetServer() = default;

  void StrictOneWay() override { controller_binding_->events().ReceivedStrictOneWay(); }

  void FlexibleOneWay() override { controller_binding_->events().ReceivedFlexibleOneWay(); }

  void StrictTwoWay(StrictTwoWayCallback callback) override { callback(); }

  void StrictTwoWayFields(int32_t reply_with, StrictTwoWayFieldsCallback callback) override {
    callback(reply_with);
  }

  void StrictTwoWayErr(::fidl::serversuite::OpenTargetStrictTwoWayErrRequest request,
                       StrictTwoWayErrCallback callback) override {
    if (request.is_reply_success()) {
      callback(fidl::serversuite::OpenTarget_StrictTwoWayErr_Result::WithResponse({}));
    } else if (request.is_reply_error()) {
      callback(fidl::serversuite::OpenTarget_StrictTwoWayErr_Result::WithErr(
          std::move(request.reply_error())));
    } else {
      ZX_PANIC("Unrecognized StrictTwoWayErr Request Variant");
    }
  }

  void StrictTwoWayFieldsErr(::fidl::serversuite::OpenTargetStrictTwoWayFieldsErrRequest request,
                             StrictTwoWayFieldsErrCallback callback) override {
    if (request.is_reply_success()) {
      callback(fidl::serversuite::OpenTarget_StrictTwoWayFieldsErr_Result::WithResponse(
          fidl::serversuite::OpenTarget_StrictTwoWayFieldsErr_Response(request.reply_success())));
    } else if (request.is_reply_error()) {
      callback(fidl::serversuite::OpenTarget_StrictTwoWayFieldsErr_Result::WithErr(
          std::move(request.reply_error())));
    } else {
      ZX_PANIC("Unrecognized StrictTwoWayFieldsErr Request Variant");
    }
  }

  void FlexibleTwoWay(FlexibleTwoWayCallback callback) override {
    callback(fidl::serversuite::OpenTarget_FlexibleTwoWay_Result::WithResponse({}));
  }

  void FlexibleTwoWayFields(int32_t reply_with, FlexibleTwoWayFieldsCallback callback) override {
    callback(fidl::serversuite::OpenTarget_FlexibleTwoWayFields_Result::WithResponse(
        fidl::serversuite::OpenTarget_FlexibleTwoWayFields_Response(reply_with)));
  }

  void FlexibleTwoWayErr(::fidl::serversuite::OpenTargetFlexibleTwoWayErrRequest request,
                         FlexibleTwoWayErrCallback callback) override {
    if (request.is_reply_success()) {
      callback(fidl::serversuite::OpenTarget_FlexibleTwoWayErr_Result::WithResponse({}));
    } else if (request.is_reply_error()) {
      callback(fidl::serversuite::OpenTarget_FlexibleTwoWayErr_Result::WithErr(
          std::move(request.reply_error())));
    } else {
      ZX_PANIC("Unrecognized FlexibleTwoWayErr Request Variant");
    }
  }

  void FlexibleTwoWayFieldsErr(
      ::fidl::serversuite::OpenTargetFlexibleTwoWayFieldsErrRequest request,
      FlexibleTwoWayFieldsErrCallback callback) override {
    if (request.is_reply_success()) {
      callback(fidl::serversuite::OpenTarget_FlexibleTwoWayFieldsErr_Result::WithResponse(
          fidl::serversuite::OpenTarget_FlexibleTwoWayFieldsErr_Response(request.reply_success())));
    } else if (request.is_reply_error()) {
      callback(fidl::serversuite::OpenTarget_FlexibleTwoWayFieldsErr_Result::WithErr(
          std::move(request.reply_error())));
    } else {
      ZX_PANIC("Unrecognized FlexibleTwoWayFieldsErr Request Variant");
    }
  }

  void set_controller_binding(
      fidl::Binding<fidl::serversuite::OpenTargetController>* controller_binding) {
    controller_binding_ = controller_binding;
  }

 protected:
  void handle_unknown_method(uint64_t ordinal, bool method_has_response) override {
    auto unknown_method_type = method_has_response ? fidl::serversuite::UnknownMethodType::TWO_WAY
                                                   : fidl::serversuite::UnknownMethodType::ONE_WAY;
    controller_binding_->events().ReceivedUnknownMethod(ordinal, unknown_method_type);
  }

 private:
  fidl::Binding<fidl::serversuite::OpenTargetController>* controller_binding_ = nullptr;
};

template <>
struct TargetTypes<fidl::serversuite::AnyTarget::Tag::kOpenTarget> {
  using Controller = fidl::serversuite::OpenTargetController;
  using Sut = fidl::serversuite::OpenTarget;

  using ControllerServer = OpenTargetControllerServer;
  using SutServer = OpenTargetServer;

  using ServerPair = fidl::serversuite::OpenTargetServerPair;
};

class ActiveServerBase {
 public:
  virtual ~ActiveServerBase() = default;
};

template <fidl::serversuite::AnyTarget::Tag TARGET>
class ActiveServer : public ActiveServerBase {
 public:
  using Controller = typename TargetTypes<TARGET>::Controller;
  using Sut = typename TargetTypes<TARGET>::Sut;
  using ControllerServer = typename TargetTypes<TARGET>::ControllerServer;
  using SutServer = typename TargetTypes<TARGET>::SutServer;
  using ServerPair = typename TargetTypes<TARGET>::ServerPair;

  ActiveServer() = default;
  ~ActiveServer() override = default;

  void Bind(ServerPair& server_pair, async_dispatcher_t* dispatcher) {
    sut_binding_->set_error_handler([this](zx_status_t status) {
      controller_binding_->events().WillTeardown(fidl::serversuite::TeardownReason::OTHER);
    });

    controller_binding_->Bind(std::move(server_pair.controller), dispatcher);
    sut_binding_->Bind(std::move(server_pair.sut), dispatcher);
  }

  std::unique_ptr<ControllerServer> controller_server_ = std::make_unique<ControllerServer>();
  std::unique_ptr<SutServer> sut_server_ = std::make_unique<SutServer>();
  std::unique_ptr<fidl::Binding<Controller>> controller_binding_ =
      std::make_unique<fidl::Binding<Controller>>(controller_server_.get());
  std::unique_ptr<fidl::Binding<Sut>> sut_binding_ =
      std::make_unique<fidl::Binding<Sut>>(sut_server_.get());
};

class RunnerServer : public fidl::serversuite::Runner {
 public:
  explicit RunnerServer(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  void IsTestEnabled(fidl::serversuite::Test test, IsTestEnabledCallback callback) override {
    switch (test) {
      case fidl::serversuite::Test::IGNORE_DISABLED:
        // This case will forever be false, as it is intended to validate the "test disabling"
        // functionality of the runner itself.
        callback(false);
        return;

      case fidl::serversuite::Test::SERVER_SENDS_TOO_FEW_RIGHTS:
      case fidl::serversuite::Test::RESPONSE_EXCEEDS_BYTE_LIMIT:
      case fidl::serversuite::Test::RESPONSE_EXCEEDS_HANDLE_LIMIT:
        callback(false);
        return;

      case fidl::serversuite::Test::V1_TWO_WAY_NO_PAYLOAD:
      case fidl::serversuite::Test::V1_TWO_WAY_STRUCT_PAYLOAD:
        // TODO(fxbug.dev/99738): HLCPP bindings should reject V1 wire format.
        callback(false);
        return;

      default:
        callback(true);
        return;
    }
  }

  void IsTeardownReasonSupported(IsTeardownReasonSupportedCallback callback) override {
    callback(false);
  }

  void Start(fidl::serversuite::AnyTarget target, StartCallback callback) override {
    if (target.is_closed_target()) {
      auto active_server =
          std::make_unique<ActiveServer<fidl::serversuite::AnyTarget::Tag::kClosedTarget>>();

      active_server->controller_server_->set_sut_binding(active_server->sut_binding_.get());
      active_server->sut_server_->set_controller_binding(active_server->controller_binding_.get());

      active_server->Bind(target.closed_target(), dispatcher_);
      active_server_ = std::move(active_server);

      callback();
    } else if (target.is_ajar_target()) {
      auto active_server =
          std::make_unique<ActiveServer<fidl::serversuite::AnyTarget::Tag::kAjarTarget>>();

      active_server->sut_server_->set_controller_binding(active_server->controller_binding_.get());

      active_server->Bind(target.ajar_target(), dispatcher_);
      active_server_ = std::move(active_server);
      callback();
    } else if (target.is_open_target()) {
      auto active_server =
          std::make_unique<ActiveServer<fidl::serversuite::AnyTarget::Tag::kOpenTarget>>();

      active_server->controller_server_->set_sut_binding(active_server->sut_binding_.get());
      active_server->sut_server_->set_controller_binding(active_server->controller_binding_.get());

      active_server->Bind(target.open_target(), dispatcher_);
      active_server_ = std::move(active_server);
      callback();
    } else {
      ZX_PANIC("Unrecognized target type.");
    }
  }

  void CheckAlive(CheckAliveCallback callback) override { return callback(); }

 private:
  async_dispatcher_t* dispatcher_;
  std::unique_ptr<ActiveServerBase> active_server_;
};

int main(int argc, const char** argv) {
  std::cout << "HLCPP server: main" << std::endl;
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  RunnerServer server(loop.dispatcher());
  fidl::Binding<fidl::serversuite::Runner> binding(&server);
  fidl::InterfaceRequestHandler<fidl::serversuite::Runner> handler =
      [&](fidl::InterfaceRequest<fidl::serversuite::Runner> server_end) {
        binding.Bind(std::move(server_end));
      };
  auto context = sys::ComponentContext::CreateAndServeOutgoingDirectory();
  context->outgoing()->AddPublicService(std::move(handler));

  std::cout << "HLCPP server: ready!" << std::endl;
  return loop.Run();
}
