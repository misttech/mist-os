// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fidl.serversuite/cpp/wire_messaging.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include <iostream>

#include "src/tests/fidl/server_suite/cpp_util/error_util.h"

class ClosedTargetControllerServer
    : public fidl::WireServer<fidl_serversuite::ClosedTargetController> {
 public:
  ClosedTargetControllerServer() = default;

  void CloseWithEpitaph(CloseWithEpitaphRequestView request,
                        CloseWithEpitaphCompleter::Sync& completer) override {
    sut_binding_->Close(request->epitaph_status);
  }

  void set_sut_binding(fidl::ServerBindingRef<fidl_serversuite::ClosedTarget> sut_binding) {
    ZX_ASSERT_MSG(!sut_binding_, "sut binding already set");
    sut_binding_ = std::move(sut_binding);
  }

 private:
  std::optional<fidl::ServerBindingRef<fidl_serversuite::ClosedTarget>> sut_binding_;
};

class ClosedTargetServer : public fidl::WireServer<fidl_serversuite::ClosedTarget> {
 public:
  ClosedTargetServer() = default;

  void OneWayNoPayload(OneWayNoPayloadCompleter::Sync& completer) override {
    std::cout << "Target.OneWayNoPayload()" << std::endl;
    auto result = fidl::WireSendEvent(controller_binding_.value())->ReceivedOneWayNoPayload();
    ZX_ASSERT(result.ok());
  }

  void TwoWayNoPayload(TwoWayNoPayloadCompleter::Sync& completer) override {
    std::cout << "Target.TwoWayNoPayload()" << std::endl;
    completer.Reply();
  }

  void TwoWayStructPayload(TwoWayStructPayloadRequestView request,
                           TwoWayStructPayloadCompleter::Sync& completer) override {
    std::cout << "Target.TwoWayStructPayload()" << std::endl;
    completer.Reply(request->v);
  }

  void TwoWayTablePayload(TwoWayTablePayloadRequestView request,
                          TwoWayTablePayloadCompleter::Sync& completer) override {
    std::cout << "Target.TwoWayTablePayload()" << std::endl;
    fidl::Arena arena;
    completer.Reply(fidl_serversuite::wire::ClosedTargetTwoWayTablePayloadResponse::Builder(arena)
                        .v(request->v())
                        .Build());
  }

  void TwoWayUnionPayload(TwoWayUnionPayloadRequestView request,
                          TwoWayUnionPayloadCompleter::Sync& completer) override {
    std::cout << "Target.TwoWayUnionPayload()" << std::endl;
    completer.Reply(
        fidl_serversuite::wire::ClosedTargetTwoWayUnionPayloadResponse::WithV(request->v()));
  }

  void TwoWayResult(TwoWayResultRequestView request,
                    TwoWayResultCompleter::Sync& completer) override {
    std::cout << "Target.TwoWayResult()" << std::endl;
    switch (request->Which()) {
      case fidl_serversuite::wire::ClosedTargetTwoWayResultRequest::Tag::kPayload: {
        completer.ReplySuccess(request->payload());
        return;
      }
      case fidl_serversuite::wire::ClosedTargetTwoWayResultRequest::Tag::kError:
        completer.ReplyError(request->error());
        return;
    }
    ZX_PANIC("unhandled case");
  }

  void GetHandleRights(GetHandleRightsRequestView request,
                       GetHandleRightsCompleter::Sync& completer) override {
    zx_info_handle_basic_t info;
    ZX_ASSERT(ZX_OK == request->handle.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr,
                                                nullptr));
    completer.Reply(info.rights);
  }

  void GetSignalableEventRights(GetSignalableEventRightsRequestView request,
                                GetSignalableEventRightsCompleter::Sync& completer) override {
    zx_info_handle_basic_t info;
    ZX_ASSERT(ZX_OK == request->handle.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr,
                                                nullptr));
    completer.Reply(info.rights);
  }

  void EchoAsTransferableSignalableEvent(
      EchoAsTransferableSignalableEventRequestView request,
      EchoAsTransferableSignalableEventCompleter::Sync& completer) override {
    completer.Reply(zx::event(request->handle.release()));
  }

  void ByteVectorSize(ByteVectorSizeRequestView request,
                      ByteVectorSizeCompleter::Sync& completer) override {
    completer.Reply(static_cast<uint32_t>(request->vec.count()));
  }

  void HandleVectorSize(HandleVectorSizeRequestView request,
                        HandleVectorSizeCompleter::Sync& completer) override {
    completer.Reply(static_cast<uint32_t>(request->vec.count()));
  }

  void CreateNByteVector(CreateNByteVectorRequestView request,
                         CreateNByteVectorCompleter::Sync& completer) override {
    std::vector<uint8_t> bytes(request->n);
    completer.Reply(fidl::VectorView<uint8_t>::FromExternal(bytes));
  }

  void CreateNHandleVector(CreateNHandleVectorRequestView request,
                           CreateNHandleVectorCompleter::Sync& completer) override {
    std::vector<zx::event> handles(request->n);
    for (auto& handle : handles) {
      ZX_ASSERT(ZX_OK == zx::event::create(0, &handle));
    }
    completer.Reply(fidl::VectorView<zx::event>::FromExternal(handles));
  }

  void OnUnbound(fidl::UnbindInfo info, fidl::ServerEnd<fidl_serversuite::ClosedTarget>) {
    if (!info.is_dispatcher_shutdown() && !info.is_user_initiated() && !info.is_peer_closed()) {
      std::cout << "ClosedTarget unbound with error: " << info.FormatDescription() << std::endl;
    }
    (void)fidl::WireSendEvent(controller_binding_.value())
        ->WillTeardown(servertest_util::ClassifyTeardownReason(info));
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

class AjarTargetServer : public fidl::WireServer<fidl_serversuite::AjarTarget> {
 public:
  explicit AjarTargetServer(fidl::ServerEnd<fidl_serversuite::AjarTargetController> controller)
      : controller_(std::move(controller)) {}

  void handle_unknown_method(fidl::UnknownMethodMetadata<fidl_serversuite::AjarTarget> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    auto result = fidl::WireSendEvent(controller_)
                      ->ReceivedUnknownMethod(metadata.method_ordinal,
                                              fidl_serversuite::wire::UnknownMethodType::kOneWay);
    ZX_ASSERT(result.ok());
  }

  void OnUnbound(fidl::UnbindInfo info, fidl::ServerEnd<fidl_serversuite::AjarTarget>) {
    if (!info.is_dispatcher_shutdown() && !info.is_user_initiated() && !info.is_peer_closed()) {
      std::cout << "AjarTarget unbound with error: " << info.FormatDescription() << std::endl;
    }
    (void)fidl::WireSendEvent(controller_)
        ->WillTeardown(servertest_util::ClassifyTeardownReason(info));
  }

 private:
  fidl::ServerEnd<fidl_serversuite::AjarTargetController> controller_;
};

class OpenTargetControllerServer : public fidl::WireServer<fidl_serversuite::OpenTargetController> {
 public:
  OpenTargetControllerServer() = default;

  void SendStrictEvent(SendStrictEventCompleter::Sync& completer) override {
    auto result = fidl::WireSendEvent(sut_binding_.value())->StrictEvent();
    if (result.ok()) {
      completer.ReplySuccess();
    } else {
      completer.ReplyError(servertest_util::ClassifySendEventError(result));
    }
  }

  void SendFlexibleEvent(SendFlexibleEventCompleter::Sync& completer) override {
    auto result = fidl::WireSendEvent(sut_binding_.value())->FlexibleEvent();
    if (result.ok()) {
      completer.ReplySuccess();
    } else {
      completer.ReplyError(servertest_util::ClassifySendEventError(result));
    }
  }

  void set_sut_binding(fidl::ServerBindingRef<fidl_serversuite::OpenTarget> sut_binding) {
    ZX_ASSERT_MSG(!sut_binding_, "sut binding already set");
    sut_binding_ = std::move(sut_binding);
  }

 private:
  std::optional<fidl::ServerBindingRef<fidl_serversuite::OpenTarget>> sut_binding_;
};

class OpenTargetServer : public fidl::WireServer<fidl_serversuite::OpenTarget> {
 public:
  OpenTargetServer() = default;

  void StrictOneWay(StrictOneWayCompleter::Sync& completer) override {
    auto result = fidl::WireSendEvent(controller_binding_.value())->ReceivedStrictOneWay();
    ZX_ASSERT(result.ok());
  }

  void FlexibleOneWay(FlexibleOneWayCompleter::Sync& completer) override {
    auto result = fidl::WireSendEvent(controller_binding_.value())->ReceivedFlexibleOneWay();
    ZX_ASSERT(result.ok());
  }

  void StrictTwoWay(StrictTwoWayCompleter::Sync& completer) override { completer.Reply(); }

  void StrictTwoWayFields(StrictTwoWayFieldsRequestView request,
                          StrictTwoWayFieldsCompleter::Sync& completer) override {
    completer.Reply(request->reply_with);
  }

  void StrictTwoWayErr(StrictTwoWayErrRequestView request,
                       StrictTwoWayErrCompleter::Sync& completer) override {
    switch (request->Which()) {
      case fidl_serversuite::wire::OpenTargetStrictTwoWayErrRequest::Tag::kReplySuccess:
        completer.ReplySuccess();
        break;
      case fidl_serversuite::wire::OpenTargetStrictTwoWayErrRequest::Tag::kReplyError:
        completer.ReplyError(request->reply_error());
        break;
    }
  }

  void StrictTwoWayFieldsErr(StrictTwoWayFieldsErrRequestView request,
                             StrictTwoWayFieldsErrCompleter::Sync& completer) override {
    switch (request->Which()) {
      case fidl_serversuite::wire::OpenTargetStrictTwoWayFieldsErrRequest::Tag::kReplySuccess:
        completer.ReplySuccess(request->reply_success());
        break;
      case fidl_serversuite::wire::OpenTargetStrictTwoWayFieldsErrRequest::Tag::kReplyError:
        completer.ReplyError(request->reply_error());
        break;
    }
  }

  void FlexibleTwoWay(FlexibleTwoWayCompleter::Sync& completer) override { completer.Reply(); }

  void FlexibleTwoWayFields(FlexibleTwoWayFieldsRequestView request,
                            FlexibleTwoWayFieldsCompleter::Sync& completer) override {
    completer.Reply(request->reply_with);
  }

  void FlexibleTwoWayErr(FlexibleTwoWayErrRequestView request,
                         FlexibleTwoWayErrCompleter::Sync& completer) override {
    switch (request->Which()) {
      case fidl_serversuite::wire::OpenTargetFlexibleTwoWayErrRequest::Tag::kReplySuccess:
        completer.ReplySuccess();
        break;
      case fidl_serversuite::wire::OpenTargetFlexibleTwoWayErrRequest::Tag::kReplyError:
        completer.ReplyError(request->reply_error());
        break;
    }
  }

  void FlexibleTwoWayFieldsErr(FlexibleTwoWayFieldsErrRequestView request,
                               FlexibleTwoWayFieldsErrCompleter::Sync& completer) override {
    switch (request->Which()) {
      case fidl_serversuite::wire::OpenTargetFlexibleTwoWayFieldsErrRequest::Tag::kReplySuccess:
        completer.ReplySuccess(request->reply_success());
        break;
      case fidl_serversuite::wire::OpenTargetFlexibleTwoWayFieldsErrRequest::Tag::kReplyError:
        completer.ReplyError(request->reply_error());
        break;
    }
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fidl_serversuite::OpenTarget> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    fidl_serversuite::wire::UnknownMethodType method_type;
    switch (metadata.unknown_method_type) {
      case fidl::UnknownMethodType::kOneWay:
        method_type = fidl_serversuite::wire::UnknownMethodType::kOneWay;
        break;
      case fidl::UnknownMethodType::kTwoWay:
        method_type = fidl_serversuite::wire::UnknownMethodType::kTwoWay;
        break;
    }
    auto result = fidl::WireSendEvent(controller_binding_.value())
                      ->ReceivedUnknownMethod(metadata.method_ordinal, method_type);
    ZX_ASSERT(result.ok());
  }

  void OnUnbound(fidl::UnbindInfo info, fidl::ServerEnd<fidl_serversuite::OpenTarget>) {
    if (!info.is_dispatcher_shutdown() && !info.is_user_initiated() && !info.is_peer_closed()) {
      std::cout << "OpenTarget unbound with error: " << info.FormatDescription() << std::endl;
    }
    (void)fidl::WireSendEvent(controller_binding_.value())
        ->WillTeardown(servertest_util::ClassifyTeardownReason(info));
  }

  void set_controller_binding(
      fidl::ServerBindingRef<fidl_serversuite::OpenTargetController> controller_binding) {
    ZX_ASSERT_MSG(!controller_binding_, "controller binding already set");
    controller_binding_ = std::move(controller_binding);
  }

 private:
  std::optional<fidl::ServerBindingRef<fidl_serversuite::OpenTargetController>> controller_binding_;
};

class LargeMessageTargetServer : public fidl::WireServer<fidl_serversuite::LargeMessageTarget> {
 public:
  explicit LargeMessageTargetServer(
      fidl::ServerEnd<fidl_serversuite::LargeMessageTargetController> controller)
      : controller_(std::move(controller)) {}

  void DecodeBoundedKnownToBeSmall(::fidl_serversuite::wire::BoundedKnownToBeSmall* request,
                                   DecodeBoundedKnownToBeSmallCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.DecodeBoundedKnownToBeSmall()" << std::endl;
    auto result =
        fidl::WireSendEvent(controller_)
            ->ReceivedOneWay(
                fidl_serversuite::LargeMessageTargetOneWayMethod::kDecodeBoundedKnownToBeSmall);
    ZX_ASSERT(result.ok());
  }
  void DecodeBoundedMaybeLarge(::fidl_serversuite::wire::BoundedMaybeLarge* request,
                               DecodeBoundedMaybeLargeCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.DecodeBoundedMaybeLarge()" << std::endl;
    auto result =
        fidl::WireSendEvent(controller_)
            ->ReceivedOneWay(
                fidl_serversuite::LargeMessageTargetOneWayMethod::kDecodeBoundedMaybeLarge);
    ZX_ASSERT(result.ok());
  }
  void DecodeSemiBoundedBelievedToBeSmall(
      ::fidl_serversuite::wire::SemiBoundedBelievedToBeSmall* request,
      DecodeSemiBoundedBelievedToBeSmallCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.DecodeSemiBoundedBelievedToBeSmall()" << std::endl;
    auto result = fidl::WireSendEvent(controller_)
                      ->ReceivedOneWay(fidl_serversuite::LargeMessageTargetOneWayMethod::
                                           kDecodeSemiBoundedBelievedToBeSmall);
    ZX_ASSERT(result.ok());
  }
  void DecodeSemiBoundedMaybeLarge(::fidl_serversuite::wire::SemiBoundedMaybeLarge* request,
                                   DecodeSemiBoundedMaybeLargeCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.DecodeSemiBoundedMaybeLarge()" << std::endl;
    auto result =
        fidl::WireSendEvent(controller_)
            ->ReceivedOneWay(
                fidl_serversuite::LargeMessageTargetOneWayMethod::kDecodeSemiBoundedMaybeLarge);
    ZX_ASSERT(result.ok());
  }
  void DecodeUnboundedMaybeLargeValue(
      ::fidl_serversuite::wire::UnboundedMaybeLargeValue* request,
      DecodeUnboundedMaybeLargeValueCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.DecodeUnboundedMaybeLargeValue()" << std::endl;
    auto result =
        fidl::WireSendEvent(controller_)
            ->ReceivedOneWay(
                fidl_serversuite::LargeMessageTargetOneWayMethod::kDecodeUnboundedMaybeLargeValue);
    ZX_ASSERT(result.ok());
  }
  void DecodeUnboundedMaybeLargeResource(
      ::fidl_serversuite::wire::UnboundedMaybeLargeResource* request,
      DecodeUnboundedMaybeLargeResourceCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.DecodeUnboundedMaybeLargeResource()" << std::endl;
    auto result = fidl::WireSendEvent(controller_)
                      ->ReceivedOneWay(fidl_serversuite::LargeMessageTargetOneWayMethod::
                                           kDecodeUnboundedMaybeLargeResource);
    ZX_ASSERT(result.ok());
  }
  void EncodeBoundedKnownToBeSmall(::fidl_serversuite::wire::BoundedKnownToBeSmall* request,
                                   EncodeBoundedKnownToBeSmallCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.EncodeBoundedKnownToBeSmall()" << std::endl;
    completer.Reply(request->bytes);
  }
  void EncodeBoundedMaybeLarge(::fidl_serversuite::wire::BoundedMaybeLarge* request,
                               EncodeBoundedMaybeLargeCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.EncodeBoundedMaybeLarge()" << std::endl;
    completer.Reply(request->bytes);
  }
  void EncodeSemiBoundedBelievedToBeSmall(
      ::fidl_serversuite::wire::SemiBoundedBelievedToBeSmall* request,
      EncodeSemiBoundedBelievedToBeSmallCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.EncodeSemiBoundedBelievedToBeSmall()" << std::endl;
    completer.Reply(*request);
  }
  void EncodeSemiBoundedMaybeLarge(::fidl_serversuite::wire::SemiBoundedMaybeLarge* request,
                                   EncodeSemiBoundedMaybeLargeCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.EncodeSemiBoundedMaybeLarge()" << std::endl;
    completer.Reply(*request);
  }
  void EncodeUnboundedMaybeLargeValue(
      ::fidl_serversuite::wire::UnboundedMaybeLargeValue* request,
      EncodeUnboundedMaybeLargeValueCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.EncodeUnboundedMaybeLargeValue()" << std::endl;
    completer.Reply(request->bytes);
  }
  void EncodeUnboundedMaybeLargeResource(
      ::fidl_serversuite::wire::LargeMessageTargetEncodeUnboundedMaybeLargeResourceRequest* request,
      EncodeUnboundedMaybeLargeResourceCompleter::Sync& completer) override {
    std::cout << "LargeMessageTarget.EncodeUnboundedMaybeLargeResource()" << std::endl;
    // TODO(fxbug.dev/114263): Support populating unset handles.
    completer.Reply(std::move(request->data.elements));
  }

  void OnUnbound(fidl::UnbindInfo info, fidl::ServerEnd<fidl_serversuite::LargeMessageTarget>) {
    if (!info.is_dispatcher_shutdown() && !info.is_user_initiated() && !info.is_peer_closed()) {
      std::cout << "ClosedTarget unbound with error: " << info.FormatDescription() << std::endl;
    }
    (void)fidl::WireSendEvent(controller_)
        ->WillTeardown(servertest_util::ClassifyTeardownReason(info));
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fidl_serversuite::LargeMessageTarget> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    fidl_serversuite::wire::UnknownMethodType method_type;
    switch (metadata.unknown_method_type) {
      case fidl::UnknownMethodType::kOneWay:
        method_type = fidl_serversuite::wire::UnknownMethodType::kOneWay;
        break;
      case fidl::UnknownMethodType::kTwoWay:
        method_type = fidl_serversuite::wire::UnknownMethodType::kTwoWay;
        break;
    }
    auto result = fidl::WireSendEvent(controller_)
                      ->ReceivedUnknownMethod(metadata.method_ordinal, method_type);
    ZX_ASSERT(result.ok());
  }

 private:
  fidl::ServerEnd<fidl_serversuite::LargeMessageTargetController> controller_;
};

class RunnerServer : public fidl::WireServer<fidl_serversuite::Runner> {
 public:
  explicit RunnerServer(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  void IsTestEnabled(IsTestEnabledRequestView request,
                     IsTestEnabledCompleter::Sync& completer) override {
    bool is_enabled = [&request]() {
      switch (request->test) {
        case fidl_serversuite::Test::kIgnoreDisabled:
          // This case will forever be false, as it is intended to validate the "test disabling"
          // functionality of the runner itself.
          return false;

        case fidl_serversuite::Test::kEventSendingDoNotReportPeerClosed:
        case fidl_serversuite::Test::kOneWayWithNonZeroTxid:
        case fidl_serversuite::Test::kTwoWayNoPayloadWithZeroTxid:
          return false;

        case fidl_serversuite::Test::kGoodDecodeBoundedMaybeLargeMessage:
        case fidl_serversuite::Test::kGoodDecodeSemiBoundedUnknowableLargeMessage:
        case fidl_serversuite::Test::kGoodDecodeSemiBoundedMaybeLargeMessage:
        case fidl_serversuite::Test::kGoodDecodeUnboundedLargeMessage:
        case fidl_serversuite::Test::kGoodDecode63HandleLargeMessage:
        case fidl_serversuite::Test::kGoodDecodeUnknownLargeMessage:
        case fidl_serversuite::Test::kBadDecodeByteOverflowFlagSetOnBoundedSmallMessage:
        case fidl_serversuite::Test::kBadDecodeByteOverflowFlagSetOnUnboundedSmallMessage:
        case fidl_serversuite::Test::kBadDecodeByteOverflowFlagUnsetOnUnboundedLargeMessage:
        case fidl_serversuite::Test::kBadDecodeLargeMessageInfoOmitted:
        case fidl_serversuite::Test::kBadDecodeLargeMessageInfoTooSmall:
        case fidl_serversuite::Test::kBadDecodeLargeMessageInfoTooLarge:
        case fidl_serversuite::Test::kBadDecodeLargeMessageInfoTopHalfUnzeroed:
        case fidl_serversuite::Test::kBadDecodeLargeMessageInfoByteCountIsZero:
        case fidl_serversuite::Test::kBadDecodeLargeMessageInfoByteCountBelowMinimum:
        case fidl_serversuite::Test::kBadDecodeLargeMessageNoHandles:
        case fidl_serversuite::Test::kBadDecodeLargeMessageTooFewHandles:
        case fidl_serversuite::Test::kBadDecodeLargeMessage64Handles:
        case fidl_serversuite::Test::kBadDecodeLargeMessageLastHandleNotVmo:
        case fidl_serversuite::Test::kBadDecodeLargeMessageLastHandleInsufficientRights:
        case fidl_serversuite::Test::kBadDecodeLargeMessageLastHandleExcessiveRights:
        case fidl_serversuite::Test::kBadDecodeLargeMessageVmoTooSmall:
        case fidl_serversuite::Test::kBadDecodeLargeMessageInfoByteCountTooSmall:
        case fidl_serversuite::Test::kBadDecodeLargeMessageInfoByteCountTooLarge:
          // TODO(fxbug.dev/114261): Test decoding large messages.
          return false;

        case fidl_serversuite::Test::kGoodEncodeBoundedMaybeLargeMessage:
        case fidl_serversuite::Test::kGoodEncodeSemiBoundedMaybeLargeMessage:
        case fidl_serversuite::Test::kGoodEncodeUnboundedLargeMessage:
        case fidl_serversuite::Test::kGoodEncode63HandleLargeMessage:
        case fidl_serversuite::Test::kBadEncode64HandleLargeMessage:
          // TODO(fxbug.dev/114263): Test encoding large messages.
          return false;

        default:
          return true;
      }
    }();
    completer.Reply(is_enabled);
  }

  void IsTeardownReasonSupported(IsTeardownReasonSupportedCompleter::Sync& completer) override {
    completer.Reply(true);
  }

  void Start(StartRequestView request, StartCompleter::Sync& completer) override {
    std::cout << "Runner.Start()" << std::endl;

    switch (request->target.Which()) {
      case ::fidl_serversuite::wire::AnyTarget::Tag::kClosedTarget: {
        auto& server_pair = request->target.closed_target();
        auto controller_server = std::make_shared<ClosedTargetControllerServer>();
        auto sut_server = std::make_shared<ClosedTargetServer>();

        auto controller_binding =
            fidl::BindServer(dispatcher_, std::move(server_pair.controller), controller_server,
                             [](auto*, fidl::UnbindInfo info, auto) {
                               if (!info.is_dispatcher_shutdown() && !info.is_user_initiated() &&
                                   !info.is_peer_closed()) {
                                 std::cerr << "ClosedTargetController unbound with error: "
                                           << info.FormatDescription() << std::endl;
                               }
                             });
        auto sut_binding = fidl::BindServer(dispatcher_, std::move(server_pair.sut), sut_server,
                                            std::mem_fn(&ClosedTargetServer::OnUnbound));

        // This is thread safe because the new server runs in the same
        // dispatcher thread as the request to start it.
        controller_server->set_sut_binding(std::move(sut_binding));
        sut_server->set_controller_binding(std::move(controller_binding));
        completer.Reply();
        break;
      }
      case ::fidl_serversuite::wire::AnyTarget::Tag::kAjarTarget: {
        auto& server_pair = request->target.ajar_target();
        auto sut_server = std::make_unique<AjarTargetServer>(std::move(server_pair.controller));
        fidl::BindServer(dispatcher_, std::move(server_pair.sut), std::move(sut_server),
                         std::mem_fn(&AjarTargetServer::OnUnbound));
        completer.Reply();
        break;
      }
      case ::fidl_serversuite::wire::AnyTarget::Tag::kOpenTarget: {
        auto& server_pair = request->target.open_target();
        auto controller_server = std::make_shared<OpenTargetControllerServer>();
        auto sut_server = std::make_shared<OpenTargetServer>();

        auto controller_binding = fidl::BindServer(
            dispatcher_, std::move(server_pair.controller), controller_server,
            [](auto*, fidl::UnbindInfo info, auto) {
              if (!info.is_dispatcher_shutdown() && !info.is_user_initiated() &&
                  !info.is_peer_closed()) {
                std::cerr << "OpenTargetController unbound with error: " << info.FormatDescription()
                          << std::endl;
              }
            });
        auto sut_binding = fidl::BindServer(dispatcher_, std::move(server_pair.sut), sut_server,
                                            std::mem_fn(&OpenTargetServer::OnUnbound));

        // This is thread safe because the new server runs in the same
        // dispatcher thread as the request to start it.
        controller_server->set_sut_binding(std::move(sut_binding));
        sut_server->set_controller_binding(std::move(controller_binding));
        completer.Reply();
        break;
      }
      case ::fidl_serversuite::wire::AnyTarget::Tag::kLargeMessageTarget: {
        auto& server_pair = request->target.large_message_target();
        auto sut_server =
            std::make_unique<LargeMessageTargetServer>(std::move(server_pair.controller));
        fidl::BindServer(dispatcher_, std::move(server_pair.sut), std::move(sut_server),
                         std::mem_fn(&LargeMessageTargetServer::OnUnbound));
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
  std::cout << "LLCPP server: main" << std::endl;
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  auto outgoing = component::OutgoingDirectory(loop.dispatcher());
  ZX_ASSERT(outgoing.ServeFromStartupInfo().is_ok());
  RunnerServer runner_server(loop.dispatcher());
  auto result = outgoing.AddProtocol<fidl_serversuite::Runner>(
      std::make_unique<RunnerServer>(loop.dispatcher()));
  ZX_ASSERT(result.is_ok());

  std::cout << "LLCPP server: ready!" << std::endl;
  return loop.Run();
}
