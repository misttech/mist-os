// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// ignore_for_file: avoid_as, dead_code, null_check_always_fails

import 'dart:async';
import 'dart:ui' as ui;

import 'package:fidl_fuchsia_ui_views/fidl_async.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:fuchsia_scenic_flutter/fuchsia_view.dart';
// ignore: implementation_imports
import 'package:mockito/mockito.dart';
import 'package:pedantic/pedantic.dart';
import 'package:zircon/zircon.dart';

void main() {
  const testViewId = 42;
  const testHandle = 10;

  Future<void> sendViewConnectedEvent(int viewId) {
    return FuchsiaViewsService.instance.platformViewChannel.binaryMessenger
        .handlePlatformMessage(
            FuchsiaViewsService.instance.platformViewChannel.name,
            FuchsiaViewsService.instance.platformViewChannel.codec
                .encodeMethodCall(MethodCall(
                    'View.viewConnected', <String, dynamic>{'viewId': viewId})),
            null);
  }

  Future<void> sendViewDisconnectedEvent(int viewId) {
    return FuchsiaViewsService.instance.platformViewChannel.binaryMessenger
        .handlePlatformMessage(
            FuchsiaViewsService.instance.platformViewChannel.name,
            FuchsiaViewsService.instance.platformViewChannel.codec
                .encodeMethodCall(MethodCall('View.viewDisconnected',
                    <String, dynamic>{'viewId': viewId})),
            null);
  }

  void expectPlatformViewChannelCall(
      WidgetTester tester, String methodName, Map<String, dynamic> methodArgs,
      {dynamic Function()? handler}) {
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        FuchsiaViewsService.instance.platformViewChannel, (call) async {
      expect(call.method, methodName);
      expect(call.arguments, methodArgs);
      if (handler != null) {
        return handler.call();
      } else {
        return null;
      }
    });
  }

  testWidgets('FuchsiaViewConnection', (WidgetTester tester) async {
    bool connectedCalled = false;
    bool disconnectedCalled = false;
    final connection = TestFuchsiaViewConnection(
      _mockViewHolderToken(testViewId),
      onViewConnected: (_) => connectedCalled = true,
      onViewDisconnected: (_) => disconnectedCalled = true,
    );

    expectPlatformViewChannelCall(tester, 'View.create', <String, dynamic>{
      'viewId': testViewId,
      'hitTestable': true,
      'focusable': true,
      'viewOcclusionHintLTRB': <double>[0, 0, 0, 0],
    });
    await connection.connect();
    expect(connection.connected, false);
    expect(connectedCalled, false);
    expect(disconnectedCalled, false);

    // signal that the platform view has connected successfully.
    await sendViewConnectedEvent(testViewId);
    expect(connection.connected, true);
    expect(connectedCalled, true);

    // signal that the platform view has disconnected.
    await sendViewDisconnectedEvent(testViewId);
    expect(connection.connected, false);
    expect(disconnectedCalled, true);
  });

  testWidgets('usePointerInjection2 generates correct platform message',
      (WidgetTester tester) async {
    final pointerX = 10.0,
        pointerY = 10.0,
        device = 1,
        traceFlowId = 1,
        timestamp = Duration(microseconds: 1);
    final pointerEvent = PointerDownEvent(
        position: Offset(pointerX, pointerY),
        device: device,
        pointer: traceFlowId,
        timeStamp: timestamp);

    ui.Size window = ui.window.physicalSize / ui.window.devicePixelRatio;

    var args = <String, dynamic>{
      'viewId': testViewId,
      'x': pointerX,
      'y': pointerY,
      'phase': 1, // EventPhase.add
      'pointerId': device,
      'traceFlowId': traceFlowId,
      'logicalWidth': window.width,
      'logicalHeight': window.height,
      'timestamp': timestamp.inMicroseconds * 1000, // nanosecs
      'viewRef': testHandle
    };

    // Correct pointer injector platform message is generated for GFX views.
    {
      final connection = TestFuchsiaViewConnection(
        _mockViewHolderToken(testViewId),
        viewRef: _mockViewRef(handleValue: testHandle),
        usePointerInjection2: true,
      );

      expectPlatformViewChannelCall(tester, 'View.create', <String, dynamic>{
        'viewId': testViewId,
        'hitTestable': true,
        'focusable': true,
        'viewOcclusionHintLTRB': <double>[0, 0, 0, 0],
      });

      await connection.connect();

      expectPlatformViewChannelCall(
          tester, 'View.pointerinjector.inject', args);

      await connection.dispatchPointerEvent(pointerEvent);
    }

    // Correct pointer injector platform message is generated for Flatland views.
    {
      final connection = TestFuchsiaViewConnection.flatland(
        _mockViewportCreationToken(testViewId),
        usePointerInjection2: true,
      );

      expectPlatformViewChannelCall(tester, 'View.create', <String, dynamic>{
        'viewId': testViewId,
        'hitTestable': true,
        'focusable': true,
        'viewOcclusionHintLTRB': <double>[0, 0, 0, 0],
      });

      await connection.connect();

      // viewRef is not present in the platform message for flatland views as
      // the flutter engine holds the viewRef and does not provide it to the
      // dart code.
      args.remove('viewRef');

      expectPlatformViewChannelCall(
          tester, 'View.pointerinjector.inject', args);

      await connection.dispatchPointerEvent(pointerEvent);
    }
  });
}

ViewRef _mockViewRef({int handleValue = 0}) {
  final handle = MockHandle();
  when(handle.isValid).thenReturn(true);
  when(handle.handle).thenReturn(handleValue);
  when(handle.duplicate(any)).thenReturn(handle);
  final eventPair = MockEventPair();
  when(eventPair.handle).thenReturn(handle);
  when(eventPair.isValid).thenReturn(true);
  final viewRef = MockViewRef();
  when(viewRef.reference).thenReturn(eventPair);

  return viewRef;
}

ViewHolderToken _mockViewHolderToken(int handleValue) {
  final handle = MockHandle();
  when(handle.handle).thenReturn(handleValue);
  when(handle.isValid).thenReturn(true);
  final eventPair = MockEventPair();
  when(eventPair.handle).thenReturn(handle);
  when(eventPair.isValid).thenReturn(true);
  final viewHolderToken = MockViewHolderToken();
  when(viewHolderToken.value).thenReturn(eventPair);

  return viewHolderToken;
}

ViewportCreationToken _mockViewportCreationToken(int handleValue) {
  final handle = MockHandle();
  when(handle.handle).thenReturn(handleValue);
  when(handle.isValid).thenReturn(true);
  final channel = MockChannel();
  when(channel.handle).thenReturn(handle);
  when(channel.isValid).thenReturn(true);
  final viewportCreationToken = MockViewportCreationToken();
  when(viewportCreationToken.value).thenReturn(channel);

  return viewportCreationToken;
}

class TestFuchsiaViewConnection extends FuchsiaViewConnection {
  TestFuchsiaViewConnection(
    ViewHolderToken viewHolderToken, {
    ViewRef? viewRef,
    FuchsiaViewConnectionCallback? onViewConnected,
    FuchsiaViewConnectionCallback? onViewDisconnected,
    FuchsiaViewConnectionStateCallback? onViewStateChanged,
    bool usePointerInjection = false,
    bool usePointerInjection2 = false,
  }) : super(
          viewHolderToken,
          viewRef: viewRef,
          onViewConnected: onViewConnected,
          onViewDisconnected: onViewDisconnected,
          onViewStateChanged: onViewStateChanged,
          usePointerInjection: usePointerInjection,
          usePointerInjection2: usePointerInjection2,
        );

  TestFuchsiaViewConnection.flatland(
    ViewportCreationToken viewportCreationToken, {
    FuchsiaViewConnectionCallback? onViewConnected,
    FuchsiaViewConnectionCallback? onViewDisconnected,
    FuchsiaViewConnectionStateCallback? onViewStateChanged,
    bool usePointerInjection = false,
    bool usePointerInjection2 = false,
  }) : super.flatland(
          viewportCreationToken,
          onViewConnected: onViewConnected,
          onViewDisconnected: onViewDisconnected,
          onViewStateChanged: onViewStateChanged,
          usePointerInjection2: usePointerInjection2,
        );

  @override
  ViewRef get hostViewRef => _mockViewRef();
}

class MockViewHolderToken extends Mock implements ViewHolderToken {
  @override
  int get hashCode => super.noSuchMethod(Invocation.method(#hashCode, []));

  @override
  bool operator ==(Object other) =>
      super.noSuchMethod(Invocation.method(#==, [other]));
}

class MockViewportCreationToken extends Mock implements ViewportCreationToken {
  @override
  int get hashCode => super.noSuchMethod(Invocation.method(#hashCode, []));

  @override
  bool operator ==(Object other) =>
      super.noSuchMethod(Invocation.method(#==, [other]));
}

class MockViewRef extends Mock implements ViewRef {
  @override
  int get hashCode => super.noSuchMethod(Invocation.method(#hashCode, []));

  @override
  bool operator ==(Object other) =>
      super.noSuchMethod(Invocation.method(#==, [other]));
}

class MockEventPair extends Mock implements EventPair {}

class MockChannel extends Mock implements Channel {}

class MockHandle extends Mock implements Handle {
  @override
  int get hashCode => super.noSuchMethod(Invocation.method(#hashCode, []));

  @override
  bool operator ==(Object other) =>
      super.noSuchMethod(Invocation.method(#==, [other]));

  @override
  Handle duplicate(int? rights) =>
      super.noSuchMethod(Invocation.method(#==, [rights]));
}
