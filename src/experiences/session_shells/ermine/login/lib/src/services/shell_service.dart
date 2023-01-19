// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'dart:async';
import 'dart:io';
import 'dart:convert' show json;
import 'package:fidl/fidl.dart';

//ignore: unused_import
import 'package:ermine_utils/ermine_utils.dart';
import 'package:fidl_fuchsia_component/fidl_async.dart';
import 'package:fidl_fuchsia_component_decl/fidl_async.dart' hide Directory;
import 'package:fidl_fuchsia_io/fidl_async.dart' hide File;
import 'package:fidl_fuchsia_ui_app/fidl_async.dart';
import 'package:fidl_fuchsia_ui_scenic/fidl_async.dart';
import 'package:fidl_fuchsia_ui_views/fidl_async.dart' hide FocusState;
import 'package:fuchsia_logger/logger.dart';
import 'package:fuchsia_scenic_flutter/fuchsia_view.dart';
import 'package:fuchsia_services/services.dart';
import 'package:flutter/material.dart';
import 'package:mobx/mobx.dart';
import 'package:zircon/zircon.dart';
import 'package:fidl_fuchsia_element/fidl_async.dart' as felement;

const kApplicationShellConfigPath =
    '/config/application_shell/application_shell.json';

const kApplicationShellCollectionName = 'application_shell';

// The component url of the ermine application shell.
const kErmineShellUrl = 'fuchsia-pkg://fuchsia.com/ermine#meta/ermine.cm';

// The component url of the gazelle application shell.
const kGazelleShellUrl =
    'fuchsia-pkg://fuchsia.com/gazelle_shell#meta/gazelle_shell.cm';

/// Defines a service to launch and support Ermine user shell.
class ShellService {
  late final StreamSubscription<bool> _focusSubscription;
  late final VoidCallback onShellReady;
  late final VoidCallback onShellExit;
  late final bool _useFlatland;
  late final String _shellComponentUrl;
  String? _overrideShellComponentUrl;

  /// A DirectoryProxy for forwarding felement.Manager connections (among
  /// others) to the application shell, once it's running.
  DirectoryProxy _shellExposedDir = DirectoryProxy();

  _ErmineViewConnection? _ermine;
  var _loadedCompleter = Completer();

  ShellService() {
    ScenicProxy scenic = ScenicProxy();
    Incoming.fromSvcPath().connectToService(scenic);
    scenic.usesFlatland().then((scenicUsesFlatland) {
      _useFlatland = scenicUsesFlatland;
      runInAction(() => {_ready.value = true});
    });
    WidgetsFlutterBinding.ensureInitialized();
    _focusSubscription = FocusState.instance.stream().listen(_onFocusChanged);

    File file = File(kApplicationShellConfigPath);
    final shellConfig = json.decode(file.readAsStringSync());
    assert(shellConfig is Map);
    assert(shellConfig['url'] is String);
    _shellComponentUrl = shellConfig['url'];
  }

  void serve(ComponentContext componentContext) {
    componentContext.outgoing
      ..addPublicService(
          (request) => Incoming.withDirectory(_shellExposedDir)
              .connectToServiceByNameWithChannel(
                  felement.Manager.$serviceName, request.passChannel()),
          felement.Manager.$serviceName)
      ..addPublicService(
          (request) => Incoming.withDirectory(_shellExposedDir)
              .connectToServiceByNameWithChannel(
                  felement.GraphicalPresenter.$serviceName,
                  request.passChannel()),
          felement.GraphicalPresenter.$serviceName);
  }

  // Override the default url of the application shell.
  set shellUrl(String url) => _overrideShellComponentUrl = url;

  String get shellUrl => _overrideShellComponentUrl ?? _shellComponentUrl;

  /// Returns [true] after call to [Scenic.usesFlatland] completes.
  bool get ready => _ready.value;
  final _ready = false.asObservable();

  /// Returns the future when shell is loaded and finished rendering a frame.
  Future get loaded => _loadedCompleter.future;

  void dispose() {
    _focusSubscription.cancel();
  }

  /// Launch Ermine shell and return [FuchsiaViewConnection].
  FuchsiaViewConnection launchErmineShell(int accountId) {
    assert(_ermine == null, 'Instance of ermine shell already exists.');

    _shellExposedDir = DirectoryProxy();

    _ermine = _ErmineViewConnection(
      useFlatland: _useFlatland,
      onReady: () {
        _loadedCompleter.complete();
        _loadedCompleter = Completer();
        onShellReady();
      },
      onExit: onShellExit,
      componentUrl: shellUrl,
      componentName: accountId.toUnsigned(64).toString(),
      exposedDir: _shellExposedDir,
      exposedDirRequest: _shellExposedDir.ctrl.request(),
    );
    return _ermine!.fuchsiaViewConnection;
  }

  void disposeErmineShell() {
    _shellExposedDir.ctrl.unbind();
    _ermine!.dispose();
    _ermine = null;
  }

  // Transfer focus to Ermine shell whenever login shell receives focus.
  void _onFocusChanged(bool focused) {
    if (focused) {
      _ermine?.setFocus();
    }
  }
}

class _ErmineViewConnection {
  final bool useFlatland;
  final VoidCallback onReady;
  final VoidCallback onExit;
  final RealmProxy realm;
  final String componentName;
  late final FuchsiaViewConnection fuchsiaViewConnection;
  bool _focusRequested = false;

  _ErmineViewConnection({
    required this.useFlatland,
    required this.onReady,
    required this.onExit,
    required String componentUrl,
    required this.componentName,
    required DirectoryProxy exposedDir,
    required InterfaceRequest<Directory> exposedDirRequest,
  }) : realm = RealmProxy() {
    // Connect to the Realm.
    Incoming.fromSvcPath().connectToService(realm);

    log.info(
        'launching application shell with url $componentUrl name $componentName');
    realm
      ..createChild(
          CollectionRef(name: kApplicationShellCollectionName),
          Child(
              name: componentName,
              url: componentUrl,
              startup: StartupMode.lazy),
          CreateChildArgs())

      // Get the shell's exposed /svc directory.
      ..openExposedDir(
          ChildRef(
              collection: kApplicationShellCollectionName, name: componentName),
          exposedDirRequest);

    // Get the ermine shell's view provider.
    final viewProvider = ViewProviderProxy();
    Incoming.withDirectory(exposedDir).connectToService(viewProvider);
    viewProvider.ctrl.whenClosed.then((_) => onExit());

    fuchsiaViewConnection = _launch(viewProvider);
  }

  void dispose() {
    realm.destroyChild(ChildRef(
        collection: kApplicationShellCollectionName, name: componentName));
  }

  void setFocus() {
    if (_focusRequested) {
      fuchsiaViewConnection.requestFocus().catchError((e) {
        log.shout(e);
      });
    }
  }

  FuchsiaViewConnection _launch(ViewProvider viewProvider) {
    if (useFlatland) {
      final viewTokens = ChannelPair();
      assert(viewTokens.status == ZX.OK);
      final viewportCreationToken =
          ViewportCreationToken(value: viewTokens.first!);
      final viewCreationToken = ViewCreationToken(value: viewTokens.second!);

      final createViewArgs =
          CreateView2Args(viewCreationToken: viewCreationToken);
      viewProvider.createView2(createViewArgs);

      return FuchsiaViewConnection.flatland(
        viewportCreationToken,
        onViewStateChanged: _onViewStateChanged,
      );
    } else {
      final viewTokens = EventPairPair();
      assert(viewTokens.status == ZX.OK);
      final viewHolderToken = ViewHolderToken(value: viewTokens.first!);
      final viewToken = ViewToken(value: viewTokens.second!);

      final viewRefPair = EventPairPair();
      final viewRef =
          ViewRef(reference: viewRefPair.first!.duplicate(ZX.RIGHTS_BASIC));
      final viewRefControl = ViewRefControl(
          reference: viewRefPair.second!
              .duplicate(ZX.DEFAULT_EVENTPAIR_RIGHTS & (~ZX.RIGHT_DUPLICATE)));
      final viewRefInject =
          ViewRef(reference: viewRefPair.first!.duplicate(ZX.RIGHTS_BASIC));

      viewProvider.createViewWithViewRef(
          viewToken.value, viewRefControl, viewRef);

      return FuchsiaViewConnection(
        viewHolderToken,
        viewRef: viewRefInject,
        onViewStateChanged: _onViewStateChanged,
      );
    }
  }

  void _onViewStateChanged(FuchsiaViewController _, bool? state) {
    if (state == true && !_focusRequested) {
      _focusRequested = true;
      setFocus();
      onReady();
    }
  }
}
