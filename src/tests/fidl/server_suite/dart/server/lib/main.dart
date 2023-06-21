// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/84961): Fix null safety and remove this language version.
// @dart=2.9

import 'dart:async';
import 'dart:core';
import 'dart:typed_data';

import 'package:fidl/fidl.dart';
import 'package:fuchsia_logger/logger.dart';
import 'package:fidl_fidl_serversuite/fidl_async.dart' hide UnknownMethodType;
import 'package:fidl_fidl_serversuite/fidl_async.dart' as serversuite;
import 'package:fuchsia_services/services.dart';
import 'package:zircon/zircon.dart';
import 'package:fidl_zx/fidl_async.dart' show Rights;

serversuite.UnknownMethodType convertUnknownMethodType(
    UnknownMethodType unknownMethodType) {
  switch (unknownMethodType) {
    case UnknownMethodType.oneWay:
      return serversuite.UnknownMethodType.oneWay;
    case UnknownMethodType.twoWay:
      return serversuite.UnknownMethodType.twoWay;
  }
}

SendEventError classifyEventError(dynamic e) {
  return SendEventError.otherError;
}

class ClosedTargetControllerImpl extends ClosedTargetController {
  ClosedTargetControllerImpl({ClosedTargetBinding sutBinding})
      : _sutBinding = sutBinding {
    _sutBinding.whenClosed.then((_) {
      _willTeardown.add(TeardownReason.other);
    });
  }

  ClosedTargetBinding _sutBinding;

  @override
  Future<void> closeWithEpitaph(int epitaphStatus) async {
    _sutBinding.close(epitaphStatus);
  }

  StreamController<TeardownReason> _willTeardown = StreamController.broadcast();

  @override
  Stream<TeardownReason> get willTeardown => _willTeardown.stream;

  void reportReceivedOneWayNoPayload() {
    _receivedOneWayNoPayload.add(null);
  }

  StreamController<void> _receivedOneWayNoPayload =
      StreamController.broadcast();

  @override
  Stream<void> get receivedOneWayNoPayload => _receivedOneWayNoPayload.stream;
}

class ClosedTargetImpl extends ClosedTarget {
  ClosedTargetImpl({ClosedTargetControllerImpl controller})
      : _controller = controller;

  final ClosedTargetControllerImpl _controller;

  @override
  Future<void> oneWayNoPayload() async {
    _controller.reportReceivedOneWayNoPayload();
  }

  @override
  Future<void> twoWayNoPayload() async {}
  @override
  Future<int> twoWayStructPayload(int v) async {
    return v;
  }

  @override
  Future<ClosedTargetTwoWayTablePayloadResponse> twoWayTablePayload(
      ClosedTargetTwoWayTablePayloadRequest payload) async {
    return ClosedTargetTwoWayTablePayloadResponse(v: payload.v);
  }

  @override
  Future<ClosedTargetTwoWayUnionPayloadResponse> twoWayUnionPayload(
      ClosedTargetTwoWayUnionPayloadRequest payload) async {
    if (payload.v == null) {
      throw ArgumentError("Request had an unknown union variant");
    }
    return ClosedTargetTwoWayUnionPayloadResponse.withV(payload.v);
  }

  @override
  Future<String> twoWayResult(ClosedTargetTwoWayResultRequest payload) async {
    if (payload.payload != null) {
      return payload.payload;
    } else if (payload.error != null) {
      throw MethodException(payload.error);
    } else {
      throw ArgumentError("Request had an unknown union variant");
    }
  }

  @override
  Future<Rights> getHandleRights(Handle handle) async {
    throw UnsupportedError(
        "Handle does not provide a method to get handle rights in Dart");
  }

  @override
  Future<Rights> getSignalableEventRights(Handle handle) {
    throw UnsupportedError(
        "Handle does not provide a method to get handle rights in Dart");
  }

  @override
  Future<Handle> echoAsTransferableSignalableEvent(Handle handle) async {
    return handle;
  }

  @override
  Future<int> byteVectorSize(Uint8List vec) async {
    return vec.length;
  }

  @override
  Future<int> handleVectorSize(List<Handle> vec) async {
    return vec.length;
  }

  @override
  Future<Uint8List> createNByteVector(int n) async {
    print('returning $n byte vector');
    return Uint8List(n);
  }

  @override
  Future<List<Handle>> createNHandleVector(int n) async {
    throw UnsupportedError("Dart does not support creating zircon Events");
  }
}

class AjarTargetControllerImpl extends AjarTargetController {
  AjarTargetControllerImpl({AjarTargetBinding sutBinding})
      : _sutBinding = sutBinding {
    _sutBinding.whenClosed.then((_) {
      _willTeardown.add(TeardownReason.other);
    });
  }
  AjarTargetBinding _sutBinding;

  StreamController<TeardownReason> _willTeardown = StreamController.broadcast();

  @override
  Stream<TeardownReason> get willTeardown => _willTeardown.stream;

  void reportReceivedUnknownMethod(
      int ordinal, serversuite.UnknownMethodType unknownMethodType) {
    _receivedUnknownMethod.add(
        AjarTargetController$ReceivedUnknownMethod$Response(
            ordinal, unknownMethodType));
  }

  StreamController<AjarTargetController$ReceivedUnknownMethod$Response>
      _receivedUnknownMethod = StreamController.broadcast();
  @override
  Stream<AjarTargetController$ReceivedUnknownMethod$Response>
      get receivedUnknownMethod => _receivedUnknownMethod.stream;
}

class AjarTargetImpl extends AjarTargetServer {
  AjarTargetImpl({AjarTargetControllerImpl controller})
      : _controller = controller;

  final AjarTargetControllerImpl _controller;

  @override
  Future<void> $unknownMethod(UnknownMethodMetadata metadata) async {
    _controller.reportReceivedUnknownMethod(
        metadata.ordinal, convertUnknownMethodType(metadata.unknownMethodType));
  }
}

class OpenTargetControllerImpl extends OpenTargetController {
  OpenTargetControllerImpl({OpenTargetBinding sutBinding})
      : _sutBinding = sutBinding {
    _sutBinding.whenClosed.then((_) {
      _willTeardown.add(TeardownReason.other);
    });
  }

  // Used to send events. Because of a circular reference, this can't be set in
  // the constructor.
  OpenTargetImpl target;

// Binding used to track when teardown happens.
  OpenTargetBinding _sutBinding;

  StreamController<TeardownReason> _willTeardown = StreamController.broadcast();

  @override
  Stream<TeardownReason> get willTeardown => _willTeardown.stream;

  @override
  Future<void> sendStrictEvent() async {
    try {
      target.sendStrictEvent();
    } catch (e) {
      throw MethodException(classifyEventError(e));
    }
  }

  @override
  Future<void> sendFlexibleEvent() async {
    try {
      target.sendFlexibleEvent();
    } catch (e) {
      throw MethodException(classifyEventError(e));
    }
  }

  void reportReceivedStrictOneWay() {
    _receivedStrictOneWay.add(null);
  }

  StreamController<void> _receivedStrictOneWay = StreamController.broadcast();
  @override
  Stream<void> get receivedStrictOneWay => _receivedStrictOneWay.stream;

  void reportReceivedFlexibleOneWay() {
    _receivedFlexibleOneWay.add(null);
  }

  StreamController<void> _receivedFlexibleOneWay = StreamController.broadcast();
  @override
  Stream<void> get receivedFlexibleOneWay => _receivedFlexibleOneWay.stream;

  void reportReceivedUnknownMethod(
      int ordinal, serversuite.UnknownMethodType unknownMethodType) {
    _receivedUnknownMethod.add(
        OpenTargetController$ReceivedUnknownMethod$Response(
            ordinal, unknownMethodType));
  }

  StreamController<OpenTargetController$ReceivedUnknownMethod$Response>
      _receivedUnknownMethod = StreamController.broadcast();
  @override
  Stream<OpenTargetController$ReceivedUnknownMethod$Response>
      get receivedUnknownMethod => _receivedUnknownMethod.stream;
}

class OpenTargetImpl extends OpenTargetServer {
  OpenTargetImpl({OpenTargetControllerImpl controller})
      : _controller = controller;

  final OpenTargetControllerImpl _controller;
  final StreamController<void> _strictEvent = StreamController.broadcast();
  final StreamController<void> _flexibleEvent = StreamController.broadcast();

  void sendStrictEvent() {
    _strictEvent.add(null);
  }

  void sendFlexibleEvent() {
    _flexibleEvent.add(null);
  }

  @override
  Stream<void> get strictEvent => _strictEvent.stream;
  @override
  Stream<void> get flexibleEvent => _flexibleEvent.stream;

  @override
  Future<void> strictOneWay() async {
    _controller.reportReceivedStrictOneWay();
  }

  @override
  Future<void> flexibleOneWay() async {
    _controller.reportReceivedFlexibleOneWay();
  }

  @override
  Future<void> strictTwoWay() async {}
  @override
  Future<int> strictTwoWayFields(int replyWith) async {
    return replyWith;
  }

  @override
  Future<void> strictTwoWayErr(OpenTargetStrictTwoWayErrRequest payload) async {
    if (payload.replySuccess != null) {
      return;
    } else if (payload.replyError != null) {
      throw MethodException(payload.replyError);
    } else {
      throw ArgumentError(
          "Request had an unknown union variant ${payload.$ordinal}");
    }
  }

  @override
  Future<int> strictTwoWayFieldsErr(
      OpenTargetStrictTwoWayFieldsErrRequest payload) async {
    if (payload.replySuccess != null) {
      return payload.replySuccess;
    } else if (payload.replyError != null) {
      throw MethodException(payload.replyError);
    } else {
      throw ArgumentError(
          "Request had an unknown union variant ${payload.$ordinal}");
    }
  }

  @override
  Future<void> flexibleTwoWay() async {}
  @override
  Future<int> flexibleTwoWayFields(int replyWith) async {
    return replyWith;
  }

  @override
  Future<void> flexibleTwoWayErr(
      OpenTargetFlexibleTwoWayErrRequest payload) async {
    if (payload.replySuccess != null) {
      return;
    } else if (payload.replyError != null) {
      throw MethodException(payload.replyError);
    } else {
      throw ArgumentError(
          "Request had an unknown union variant ${payload.$ordinal}");
    }
  }

  @override
  Future<int> flexibleTwoWayFieldsErr(
      OpenTargetFlexibleTwoWayFieldsErrRequest payload) async {
    if (payload.replySuccess != null) {
      return payload.replySuccess;
    } else if (payload.replyError != null) {
      throw MethodException(payload.replyError);
    } else {
      throw ArgumentError(
          "Request had an unknown union variant ${payload.$ordinal}");
    }
  }

  @override
  Future<void> $unknownMethod(UnknownMethodMetadata metadata) async {
    _controller.reportReceivedUnknownMethod(
        metadata.ordinal, convertUnknownMethodType(metadata.unknownMethodType));
  }
}

class RunnerImpl extends Runner {
  @override
  Future<bool> isTestEnabled(Test test) async {
    switch (test) {
      // This case will forever be false, as it is intended to validate the "test disabling"
      // functionality of the runner itself.
      case Test.ignoreDisabled:
      // Dart does not currently have APIs for explicitly retrieving handle
      // rights, so getHandleRights and getSignalableEventRights cannot be
      // implemented.
      case Test.clientSendsTooManyRights:
      case Test.clientSendsTooFewRights:
      case Test.clientSendsWrongHandleType:
      case Test.clientSendsTooFewHandles:
      case Test.clientSendsObjectOverPlainHandle:
      // Dart does not currently have APIs for creating Fuchsia Event objects,
      // so the createNHandleVector method cannot be implemented.
      case Test.responseMatchesHandleLimit:
      case Test.responseExceedsHandleLimit:
      // fxbug.dev/111266: Dart bindings don't validate TXIDs.
      case Test.oneWayWithNonZeroTxid:
      case Test.twoWayNoPayloadWithZeroTxid:
      // fxbug.dev/111299: Dart bindings don't check for channel write errors,
      // so just ignore channel errors from sending too many bytes. This causes
      // the following tests to fail from various channel errors being ignored:
      // - Ignores the ZX_ERR_OUT_OF_RANGE from sending more than
      //   ZX_CHANNEL_MAX_MSG_BYTES
      case Test.responseExceedsByteLimit:
      // - Ignores the ZX_ERR_INVALID_ARGS from sending a handle which did not
      //   have the rights specified in the handle dispositions list.
      case Test.serverSendsTooFewRights:
        return false;
      default:
        return true;
    }
  }

  Future<bool> isTeardownReasonSupported() async {
    return false;
  }

  @override
  Future<void> start(AnyTarget target) async {
    if (target.closedTarget != null) {
      var controllerBinding = ClosedTargetControllerBinding();
      var sutBinding = ClosedTargetBinding();

      var controllerServer = ClosedTargetControllerImpl(sutBinding: sutBinding);
      var sutServer = ClosedTargetImpl(controller: controllerServer);

      controllerBinding.bind(controllerServer, target.closedTarget.controller);
      sutBinding.bind(sutServer, target.closedTarget.sut);
    } else if (target.ajarTarget != null) {
      var controllerBinding = AjarTargetControllerBinding();
      var sutBinding = AjarTargetBinding();

      var controllerServer = AjarTargetControllerImpl(sutBinding: sutBinding);
      var sutServer = AjarTargetImpl(controller: controllerServer);

      controllerBinding.bind(controllerServer, target.ajarTarget.controller);
      sutBinding.bind(sutServer, target.ajarTarget.sut);
    } else if (target.openTarget != null) {
      var controllerBinding = OpenTargetControllerBinding();
      var sutBinding = OpenTargetBinding();

      var controllerServer = OpenTargetControllerImpl(sutBinding: sutBinding);
      var sutServer = OpenTargetImpl(controller: controllerServer);
      controllerServer.target = sutServer;

      controllerBinding.bind(controllerServer, target.openTarget.controller);
      sutBinding.bind(sutServer, target.openTarget.sut);
    } else {
      throw ArgumentError("Unknown AnyTarget variant: ${target.$ordinal}");
    }
  }

  Future<void> checkAlive() async {}
}

ComponentContext _context;

void main(List<String> args) {
  setupLogger(name: 'fidl-dynsuite-dart-server');
  print('Dart server: main');
  _context = ComponentContext.create();

  _context.outgoing
    ..addPublicService<Runner>((request) {
      RunnerBinding().bind(RunnerImpl(), request);
    }, Runner.$serviceName)
    ..serveFromStartupInfo();
}
