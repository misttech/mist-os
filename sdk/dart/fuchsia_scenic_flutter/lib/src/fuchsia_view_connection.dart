// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// ignore_for_file: avoid_as, null_check_always_fails, unnecessary_null_comparison

import 'package:fidl_fuchsia_ui_views/fidl_async.dart';
import 'package:flutter/widgets.dart';
import 'package:fuchsia_scenic/views.dart';

import 'fuchsia_view_controller.dart';
import 'fuchsia_views_service.dart';

/// Defines a thin wrapper around [FuchsiaViewController].
///
/// Its primary purpose is to hold on to [ViewHolderToken] for the lifetime of
/// the view controller, since [FuchsiaViewController] is agnostic to all
/// Fuchsia data types. (Eventually, [FuchsiaView] and [FuchsiaViewController]
/// will be moved to Flutter framework, which cannot have Fuchsia data types.)
class FuchsiaViewConnection extends FuchsiaViewController {
  /// The Gfx view tree token when the view is attached.
  final ViewHolderToken? viewHolderToken;

  /// The Flatland token when the view is attached.
  final ViewportCreationToken? viewportCreationToken;

  /// The handle to the view used for [requestFocus] calls.
  final ViewRef? viewRef;

  /// Callback when the connection to child's view is connected to view tree.
  final FuchsiaViewConnectionCallback? _onViewConnected;

  /// Callback when the child's view is disconnected from view tree.
  final FuchsiaViewConnectionCallback? _onViewDisconnected;

  /// Callback when the child's view render state changes.
  final FuchsiaViewConnectionStateCallback? _onViewStateChanged;

  /// Deprecated code path. Do not use.
  final bool usePointerInjection;

  /// Set to true if pointer injection into child views should be enabled using
  /// platform messages.
  final bool usePointerInjection2;

  final bool useFlatland;

  /// Constructor.
  FuchsiaViewConnection(
    this.viewHolderToken, {
    this.viewRef,
    FuchsiaViewConnectionCallback? onViewConnected,
    FuchsiaViewConnectionCallback? onViewDisconnected,
    FuchsiaViewConnectionStateCallback? onViewStateChanged,
    this.usePointerInjection = false,
    this.usePointerInjection2 = false,
    this.useFlatland = false,
  })  : assert(viewHolderToken!.value != null && viewHolderToken.value.isValid),
        assert(
            viewRef?.reference == null || viewRef!.reference.handle!.isValid),
        assert(!usePointerInjection),
        assert(!usePointerInjection2 || viewRef?.reference != null),
        viewportCreationToken = null,
        _onViewConnected = onViewConnected,
        _onViewDisconnected = onViewDisconnected,
        _onViewStateChanged = onViewStateChanged,
        super(
          viewId: viewHolderToken!.value.handle!.handle,
          onViewConnected: _handleViewConnected,
          onViewDisconnected: _handleViewDisconnected,
          onViewStateChanged: _handleViewStateChanged,
          onPointerEvent: _handlePointerEvent,
        );

  FuchsiaViewConnection.flatland(
    this.viewportCreationToken, {
    this.viewRef,
    FuchsiaViewConnectionCallback? onViewConnected,
    FuchsiaViewConnectionCallback? onViewDisconnected,
    FuchsiaViewConnectionStateCallback? onViewStateChanged,
    this.usePointerInjection2 = false,
    this.useFlatland = true,
  })  : usePointerInjection = false,
        assert(viewportCreationToken!.value != null &&
            viewportCreationToken.value.isValid),
        viewHolderToken = null,
        _onViewConnected = onViewConnected,
        _onViewDisconnected = onViewDisconnected,
        _onViewStateChanged = onViewStateChanged,
        super(
          viewId: viewportCreationToken!.value.handle!.handle,
          onViewConnected: _handleViewConnected,
          onViewDisconnected: _handleViewDisconnected,
          onViewStateChanged: _handleViewStateChanged,
          onPointerEvent: _handlePointerEvent,
        );

  /// Requests that focus be transferred to the remote Scene represented by
  /// this connection. This method is the point at which focus handling for
  /// flatland diverges. In Flatland, the Flutter engine holds the ViewRef
  /// and does not provide it to dart code, so we must refer to the child
  /// view by viewId instead
  @override
  Future<void> requestFocus([int _ = 0]) async {
    if (useFlatland) {
      return super.requestFocusById(viewId);
    } else {
      assert(viewRef?.reference != null && _ == 0);
      return super.requestFocus(viewRef!.reference.handle!.handle);
    }
  }

  static void _handleViewStateChanged(
      FuchsiaViewController controller, bool? state) async {
    FuchsiaViewConnection connection = controller as FuchsiaViewConnection;
    connection._onViewStateChanged?.call(controller, state);
  }

  @visibleForTesting
  ViewRef get hostViewRef => ScenicContext.hostViewRef();

  static void _handleViewConnected(FuchsiaViewController controller) async {
    FuchsiaViewConnection connection = controller as FuchsiaViewConnection;
    connection._onViewConnected?.call(controller);
  }

  static void _handleViewDisconnected(FuchsiaViewController controller) {
    FuchsiaViewConnection connection = controller as FuchsiaViewConnection;
    connection._onViewDisconnected?.call(controller);
  }

  static Future<void> _handlePointerEvent(
      FuchsiaViewController controller, PointerEvent pointer) async {
    FuchsiaViewConnection connection = controller as FuchsiaViewConnection;
    if (!connection.usePointerInjection2) {
      return;
    }

    if (connection.useFlatland) {
      return FuchsiaViewsService.instance
          .dispatchPointerEvent(viewId: connection.viewId, pointer: pointer);
    } else {
      assert(connection.viewRef?.reference != null);
      return FuchsiaViewsService.instance.dispatchPointerEvent(
          viewId: connection.viewId,
          pointer: pointer,
          viewRef: connection.viewRef!.reference.handle!.handle);
    }
  }
}
