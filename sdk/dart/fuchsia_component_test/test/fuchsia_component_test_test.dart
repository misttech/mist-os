// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'dart:async';

import 'package:fuchsia_component_test/realm_builder.dart';

import 'package:fidl/fidl.dart' as fidl;
import 'package:fidl_fidl_examples_routing_echo/fidl_async.dart' as fecho;
import 'package:fidl_fuchsia_component/fidl_async.dart' as fcomponent;
import 'package:fidl_fuchsia_component_test/fidl_async.dart' as fctest;
import 'package:fidl_fuchsia_component_decl/fidl_async.dart' as fdecl;
import 'package:fidl_fuchsia_io/fidl_async.dart' as fio;
import 'package:fidl_fuchsia_logger/fidl_async.dart' as flogger;

import 'package:fuchsia_logger/logger.dart';
import 'package:fuchsia_services/services.dart' as services;

import 'package:test/test.dart';

import 'dart:convert' show utf8;
import 'dart:typed_data';

const String echoClientUrl = '#meta/echo_client.cm';
const String echoServerUrl = '#meta/echo_server.cm';
const String echoClientWithBinderUrl = '#meta/echo_client_with_binder.cm';
const String echoServerWithBinderUrl = '#meta/echo_server_with_binder.cm';
const String echoClientWithBinderArg = 'Hello Fuchsia!';
const String echoClientStructuredConfigUrl = '#meta/echo_client_sc.cm';

void checkCommonExceptions(Exception err, StackTrace stacktrace) {
  if (err is fidl.MethodException<fcomponent.Error>) {
    late final String errorName;
    for (final name in fcomponent.Error.$valuesMap.keys) {
      if (err.value == fcomponent.Error.$valuesMap[name]) {
        errorName = name;
        break;
      }
    }
    log.warning('fidl.$err: fuchsia.component.Error.$errorName');
  } else if (err is fidl.MethodException<fctest.RealmBuilderError>) {
    late final String errorName;
    for (final name in fctest.RealmBuilderError.$valuesMap.keys) {
      if (err.value == fctest.RealmBuilderError.$valuesMap[name]) {
        errorName = name;
        break;
      }
    }
    log.warning('fidl.$err: fuchsia.component.test.Error.$errorName');
  } else if (err is fidl.MethodException) {
    log.warning('fidl.MethodException<${err.value.runtimeType}>($err)');
  } else if (err is fidl.FidlError) {
    log.warning('fidl.${err.runtimeType}($err), FidlErrorCode: ${err.code}');
  } else {
    log.warning('caught exception: ${err.runtimeType}($err)');
  }
  log.warning('stacktrace (if available)...\n${stacktrace.toString()}');
}

void main() {
  setupLogger(name: 'fuchsia-component-test-test');

  group('realm builder tests', () {
    group('RealmBuilder with CFv2 child', () {
      test('RealmBuilder.create()', () async {
        final builder = await RealmBuilder.create();
        expect(
          builder.addChild(
            'echoServer',
            echoServerUrl,
            ChildOptions()..eager(),
          ),
          completes,
        );
      });

      test('RealmBuilder from Component decl', () async {
        final builder = await RealmBuilder.create();
        final decl = fdecl.Component();
        expect(
          builder.addChildFromDecl(
            'componentFromDecl',
            decl,
            ChildOptions(),
          ),
          completes,
        );
      });
    });

    group('basic RealmBuilder tests', () {
      test('protocol and directory capabilities', () async {
        RealmInstance? realmInstance;
        try {
          final builder = await RealmBuilder.create();

          final echoServer = await builder.addChild(
            'echoServer',
            echoServerUrl,
          );

          await builder.addRoute(Route()
            ..capability(DirectoryCapability('hub')..rights = fio.rStarDir)
            ..from(Ref.framework())
            ..to(Ref.parent()));

          await builder.addRoute(Route()
            ..capability(ProtocolCapability(flogger.LogSink.$serviceName))
            ..from(Ref.parent())
            ..to(Ref.child(echoServer)));

          await builder.addRoute(Route()
            ..capability(ProtocolCapability(fecho.Echo.$serviceName))
            ..from(Ref.child(echoServer))
            ..to(Ref.parent()));

          realmInstance = await builder.build();

          final echo = realmInstance.root
              .connectToProtocolAtExposedDir(fecho.EchoProxy());
          const testString = 'ECHO...Echo...echo...(echo)...';

          final reply = await echo.echoString(testString);

          expect(testString, reply);
        } on Exception catch (err, stacktrace) {
          checkCommonExceptions(err, stacktrace);
          rethrow;
        } finally {
          if (realmInstance != null) {
            realmInstance.root.close();
          }
        }
      });

      test('connectToNamedProtocol', () async {
        RealmInstance? realmInstance;
        try {
          final builder = await RealmBuilder.create();

          final echoServer = await builder.addChild(
            'echoServer',
            echoServerUrl,
          );

          await builder.addRoute(Route()
            ..capability(ProtocolCapability(flogger.LogSink.$serviceName))
            ..from(Ref.parent())
            ..to(Ref.child(echoServer)));

          await builder.addRoute(Route()
            ..capability(ProtocolCapability(fecho.Echo.$serviceName))
            ..from(Ref.child(echoServer))
            ..to(Ref.parent()));

          realmInstance = await builder.build();

          final echo = fecho.EchoProxy();
          realmInstance.root.connectToNamedProtocolAtExposedDir(
            fecho.Echo.$serviceName,
            echo.ctrl.request().passChannel()!,
          );

          const testString = 'ECHO...Echo...echo...(echo)...';
          final reply = await echo.echoString(testString);
          expect(testString, reply);
        } finally {
          if (realmInstance != null) {
            realmInstance.root.close();
          }
        }
      });
    });

    test('connect to child in subrealm', () async {
      RealmInstance? realmInstance;
      try {
        const subRealmName = 'sub_realm';

        final realmBuilder = await RealmBuilder.create();
        final subRealmBuilder = await realmBuilder.addChildRealm(subRealmName);

        final echoServer = await subRealmBuilder.addChild(
          'echoServer',
          echoServerUrl,
        );

        // Route LogSink from RealmBuilder to the subRealm.
        await realmBuilder.addRoute(Route()
          ..capability(ProtocolCapability(flogger.LogSink.$serviceName))
          ..from(Ref.parent())
          ..to(Ref.childFromSubRealm(subRealmBuilder)));

        // Route LogSink from the subRealm to the Echo component.
        await subRealmBuilder.addRoute(Route()
          ..capability(ProtocolCapability(flogger.LogSink.$serviceName))
          ..from(Ref.parent())
          ..to(Ref.child(echoServer)));

        // Route the Echo service from the Echo child component to its parent
        // (the subRealm).
        await subRealmBuilder.addRoute(Route()
          ..capability(ProtocolCapability(fecho.Echo.$serviceName))
          ..from(Ref.child(echoServer))
          ..to(Ref.parent()));

        // Route the Echo service from the subRealm child to its parent
        // (the RealmBuilder).
        await realmBuilder.addRoute(Route()
          ..capability(ProtocolCapability(fecho.Echo.$serviceName))
          ..from(Ref.childFromSubRealm(subRealmBuilder))
          ..to(Ref.parent()));

        realmInstance = await realmBuilder.build();

        final echo = fecho.EchoProxy();
        realmInstance.root.connectToNamedProtocolAtExposedDir(
          fecho.Echo.$serviceName,
          echo.ctrl.request().passChannel()!,
        );

        const testString = 'ECHO...Echo...echo...(echo)...';
        final reply = await echo.echoString(testString);
        expect(testString, reply);
      } finally {
        if (realmInstance != null) {
          realmInstance.root.close();
        }
      }
    });

    test('storage', () async {
      RealmInstance? realmInstance;
      try {
        final builder = await RealmBuilder.create();

        const localStorageClientName = 'localStorageUser';

        final localChildCompleter = Completer();
        final localStorageClient = await builder.addLocalChild(
          localStorageClientName,
          options: ChildOptions()..eager(),
          onRun: (handles, onStop) async {
            var dataDir = handles.cloneFromNamespace('data');
            final exampleFile = fio.FileProxy();
            await dataDir.open(
                fio.OpenFlags.notDirectory |
                    fio.OpenFlags.rightReadable |
                    fio.OpenFlags.rightWritable |
                    fio.OpenFlags.create |
                    fio.OpenFlags.describe,
                fio.ModeType.$none,
                'example_file',
                fidl.InterfaceRequest<fio.Node>(
                    exampleFile.ctrl.request().passChannel()));

            const exampleData = 'example data';
            final encodedData = utf8.encode(exampleData);
            await exampleFile.write(utf8.encode(exampleData) as Uint8List);
            await exampleFile.seek(fio.SeekOrigin.start, 0);
            final fileContents =
                utf8.decode(await exampleFile.read(encodedData.length));

            if (exampleData != fileContents) {
              localChildCompleter.completeError(
                Exception('Wrote "$exampleData" but read back "$fileContents"'),
              );
              return;
            }
            localChildCompleter.complete(); // success!
          },
        );

        await builder.addRoute(Route()
          ..capability(StorageCapability('data')..path = '/data')
          ..from(Ref.parent())
          ..to(Ref.child(localStorageClient)));

        realmInstance = await builder.build();

        await localChildCompleter.future;
      } finally {
        if (realmInstance != null) {
          realmInstance.root.close();
        }
      }
    });

    group('API checks', () {
      test('default ChildOptions', () {
        final foptions = ChildOptions().toFidlType();
        expect(foptions.startup, fdecl.StartupMode.lazy);
        expect(foptions.environment, isNull);
        expect(foptions.onTerminate, fdecl.OnTerminate.none);
      });

      test('set ChildOptions', () {
        const envName = 'someEnv';
        final childOptions = ChildOptions()
          ..eager()
          ..rebootOnTerminate()
          ..environment = envName;
        final foptions = childOptions.toFidlType();
        expect(foptions.startup, fdecl.StartupMode.eager);
        expect(foptions.environment, envName);
        expect(foptions.onTerminate, fdecl.OnTerminate.reboot);
      });

      test('named ScopedInstance', () async {
        const collectionName = 'someCollection';
        final fac = ScopedInstanceFactory(collectionName);
        expect(fac.collectionName, collectionName);
      });

      test('ScopedInstance collectionName not found', () async {
        final fac = ScopedInstanceFactory('badCollectionName');
        ScopedInstance? scopedInstance;

        try {
          scopedInstance = await fac.newNamedInstance('someChild', 'badUrl');
        } on fidl.MethodException<fcomponent.Error> catch (err) {
          expect(err.value, fcomponent.Error.invalidArguments);
        } finally {
          expect(scopedInstance, isNull);
        }

        var caught = false;
        try {
          scopedInstance =
              await fac.newNamedInstance('someChild', '#meta/someComponent.cm');
        } on fidl.MethodException<fcomponent.Error> catch (err) {
          expect(err.value, fcomponent.Error.collectionNotFound);
          caught = true;
        } finally {
          expect(caught, true);
        }

        expect(scopedInstance, isNull);
      });
    });

    group('ScopedInstance checks', () {
      test('defaults', () async {
        final builder = await RealmBuilder.create();
        final realmInstance = await builder.build();
        final scopedInstance = realmInstance.root;
        expect(scopedInstance.collectionName, defaultCollectionName);
        expect(scopedInstance.childName, startsWith('auto-'));
        final protocol = await scopedInstance.exposedDir.query();
        expect(utf8.decode(protocol), fio.directoryProtocolName);
        scopedInstance.close();
      });
    });

    test('replace realm decl', () async {
      try {
        final builder = await RealmBuilder.create();
        var rootDecl = await builder.getRealmDecl();
        expect(rootDecl, fdecl.Component());

        final children = (rootDecl.children != null
            ? rootDecl.children!.toList()
            : <fdecl.Child>[])
          ..add(
            fdecl.Child(
              name: 'example-child',
              url: 'example://url',
              startup: fdecl.StartupMode.eager,
            ),
          );

        rootDecl = rootDecl.$cloneWith(children: fidl.Some(children));

        await builder.replaceRealmDecl(rootDecl);
        expect(rootDecl, await builder.getRealmDecl());
      } on Exception catch (err, stacktrace) {
        checkCommonExceptions(err, stacktrace);
        rethrow;
      }
    });

    test('replace component decl', () async {
      RealmInstance? realmInstance;
      try {
        final builder = await RealmBuilder.create();

        const echoServerName = 'echoServer';

        final echoServer = await builder.addChild(
          echoServerName,
          echoServerUrl,
        );

        var decl = await builder.getComponentDecl(echoServer);

        final exposes =
            (decl.exposes != null ? decl.exposes!.toList() : <fdecl.Expose>[])
              ..add(
                fdecl.Expose.withProtocol(
                  fdecl.ExposeProtocol(
                    source: fdecl.Ref.withSelf(fdecl.SelfRef()),
                    sourceName: fecho.Echo.$serviceName,
                    target: fdecl.Ref.withParent(fdecl.ParentRef()),
                    targetName: 'renamedEchoService',
                  ),
                ),
              );

        decl = decl.$cloneWith(exposes: fidl.Some(exposes));

        await builder.replaceComponentDecl(echoServer, decl);

        // Route logging to child
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(flogger.LogSink.$serviceName))
          ..from(Ref.parent())
          ..to(Ref.child(echoServer)));

        await builder.addRoute(Route()
          ..capability(ProtocolCapability('renamedEchoService'))
          ..from(Ref.child(echoServer))
          ..to(Ref.parent()));

        // Start the realmInstance. The EchoServer is not "eager", so it should
        // not start automatically.
        realmInstance = await builder.build();

        final echo = realmInstance.root.connectToProtocolAtPath(
          fecho.EchoProxy(),
          'renamedEchoService',
        );
        const testString = 'ECHO...Echo...echo...(echo)...';

        final reply = await echo.echoString(testString);

        expect(testString, reply);
      } on Exception catch (err, stacktrace) {
        checkCommonExceptions(err, stacktrace);
        rethrow;
      } finally {
        if (realmInstance != null) {
          realmInstance.root.close();
        }
      }
    });

    test('start by binding without events', () async {
      RealmInstance? realmInstance;
      try {
        final builder = await RealmBuilder.create();

        const echoClientName = 'v2Client';
        const echoClientBinder = 'echoClientBinder';
        const echoServerName = 'localEchoServer';

        // This test leverages the `echo_client` binary, with an augmented
        // component manifest that exposes `fuchsia.component.Binder`.
        //
        // The client is *NOT* started eagerly!
        //
        // The `echo_server` will automatically start if a client connects to
        // its `Echo` service. This test validates that connecting to the
        // _`echo_client_`'s `Binder` service will start the client, which will
        // subsequently invoke the `echo_server`. Since the `echo_server` is a
        // local client, the test can confirm that the echo request was made.
        final v2Client = await builder.addChild(
          echoClientName,
          echoClientWithBinderUrl,
        );

        final echoRequestReceived = Completer<String?>();

        final localEchoServer = await builder.addLocalChild(
          echoServerName,
          onRun: (handles, onStop) async {
            LocalEcho(handles, echoRequestReceived: echoRequestReceived);

            // Keep the component alive until the test is complete and the
            // realm is closed.
            await onStop.future;
          },
        );

        // Route logging to child
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(flogger.LogSink.$serviceName))
          ..from(Ref.parent())
          ..to(Ref.child(v2Client))
          ..to(Ref.child(localEchoServer)));

        // Route the echo service
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(fecho.Echo.$serviceName))
          ..from(Ref.child(localEchoServer))
          ..to(Ref.child(v2Client)));

        // Route the child's Binder service to parent, so the test can connect
        // to it to start the child.
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(fcomponent.Binder.$serviceName,
              as: echoClientBinder))
          ..from(Ref.child(v2Client))
          ..to(Ref.parent()));

        // Start the realmInstance. The child component (the client) is not
        // "eager", so it should not start automatically.
        realmInstance = await builder.build();
        final scopedInstance = realmInstance.root;

        // Start the client by binding.
        /*proxy=*/ scopedInstance.connectToProtocolAtPath(
          fcomponent.BinderProxy(),
          echoClientBinder,
        );

        final requestedEchoString = await echoRequestReceived.future;

        expect(requestedEchoString, echoClientWithBinderArg);
      } on Exception catch (err, stacktrace) {
        checkCommonExceptions(err, stacktrace);
        rethrow;
      } finally {
        if (realmInstance != null) {
          realmInstance.root.close();
        }
      }
    });

    test('start by binding with events', () async {
      RealmInstance? realmInstance;
      String? serverMoniker;
      try {
        final builder = await RealmBuilder.create();

        const serverName = 'v2Server';
        const serverBinder = 'serverBinder';

        // This test leverages the `echo_server` binary, with an augmented
        // component manifest that exposes `fuchsia.component.Binder`. The
        // `echo_server` will automatically start if a client connects to its
        // `Echo` service, but this test doesn't do that. It leverages the fact
        // that the component will launch and execute a loop waiting for
        // requests, which can make the state easier to bug if the event does
        // not arrive.
        final v2Server = await builder.addChild(
          serverName,
          echoServerWithBinderUrl,
        );

        // Route logging to child
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(flogger.LogSink.$serviceName))
          ..from(Ref.parent())
          ..to(Ref.child(v2Server)));

        // Route the child's Binder service to parent, so the test can connect
        // to it to start the child.
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(fcomponent.Binder.$serviceName,
              as: serverBinder))
          ..from(Ref.child(v2Server))
          ..to(Ref.parent()));

        // Connect to the EventStream.
        final eventStream = fcomponent.EventStreamProxy();
        await (services.Incoming.fromSvcPath()..connectToService(eventStream))
            .close();

        // Register callbacks for started events, and complete a Future when
        // called.
        final completeWaitForStart = Completer();
        unawaited(EventListener(
          eventStream,
          started: (String moniker) {
            if (moniker == serverMoniker) {
              completeWaitForStart.complete();
            }
          },
        ).listen());

        // Start the realmInstance. The child component (the server) is not
        // "eager", so it should not start automatically.
        realmInstance = await builder.build();
        final scopedInstance = realmInstance.root;
        serverMoniker = './${scopedInstance.collectionName}:'
            '${scopedInstance.childName}/$serverName';

        // Start the server
        /*serverBinder=*/ scopedInstance.connectToProtocolAtPath(
          fcomponent.BinderProxy(),
          serverBinder,
        );

        // Wait for the server "started" event
        await completeWaitForStart.future;

        // Note, since this test abruptly stopped the child component, a
        // non-zero (error) status is likely.
      } on Exception catch (err, stacktrace) {
        checkCommonExceptions(err, stacktrace);
        rethrow;
      } finally {
        if (realmInstance != null) {
          realmInstance.root.close();
        }
      }
    });

    test('route echo between two v2 components', () async {
      RealmInstance? realmInstance;
      EventListener? eventListener;
      try {
        final builder = await RealmBuilder.create();

        const echoServerName = 'v2EchoServer';
        const echoClientName = 'v2EchoClient';

        final v2EchoServer = await builder.addChild(
          echoServerName,
          echoServerUrl,
        );
        final v2EchoClient = await builder.addChild(
          echoClientName,
          echoClientUrl,
          ChildOptions()..eager(),
        );

        // Route logging to children
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(flogger.LogSink.$serviceName))
          ..from(Ref.parent())
          ..to(Ref.child(v2EchoServer))
          ..to(Ref.child(v2EchoClient)));

        // Route the echo service from server to client
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(fecho.Echo.$serviceName))
          ..from(Ref.child(v2EchoServer))
          ..to(Ref.child(v2EchoClient)));

        // Connect to the framework's EventStream.
        final eventStream = fcomponent.EventStreamProxy();
        await (services.Incoming.fromSvcPath()..connectToService(eventStream))
            .close();

        // Listen for "stopped" events.
        //
        // NOTE: This requires the test CML include a `use` for the subscribed
        // event type(s), for example:
        //
        // ```cml
        //   use: [
        //     {
        //         event_stream: [
        //             "started",
        //             "stopped",
        //         ],
        //         from: "parent",
        //     },
        //   ],
        // ```
        final completeWaitForStop = Completer<int>();
        eventListener =
            EventListener(eventStream, stopped: (String moniker, int status) {
          // Since EchoClient is [eager()], it may start and stop before the
          // async [builder.build()] completes. [realmInstance.root.childName]
          // would not be known before this stopped event is received, so
          // [endsWith()] is the best solution here.
          if (moniker.endsWith('/$echoClientName')) {
            completeWaitForStop.complete(status);
          }
        });
        unawaited(eventListener.listen());

        // Start the realm instance.
        realmInstance = await builder.build();

        // Wait for the client to stop, and check for a successful exit status.
        final stoppedStatus = await completeWaitForStop.future;
        expect(stoppedStatus, 0);
      } on Exception catch (err, stacktrace) {
        checkCommonExceptions(err, stacktrace);
        rethrow;
      } finally {
        if (eventListener != null) {
          // Ensure the listener will not trigger callbacks on component events
          // from other tests.
          eventListener.close();
        }
        if (realmInstance != null) {
          realmInstance.root.close();
        }
      }
    });

    test(
        'route echo between two v2 components with async any Event type callback',
        () async {
      RealmInstance? realmInstance;
      try {
        final builder = await RealmBuilder.create();

        const echoServerName = 'v2EchoServer';
        const echoClientName = 'v2EchoClient';

        final v2EchoServer = await builder.addChild(
          echoServerName,
          echoServerUrl,
        );
        final v2EchoClient = await builder.addChild(
          echoClientName,
          echoClientUrl,
          ChildOptions()..eager(),
        );

        // Route logging to children
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(flogger.LogSink.$serviceName))
          ..from(Ref.parent())
          ..to(Ref.child(v2EchoServer))
          ..to(Ref.child(v2EchoClient)));

        // Route the echo service from server to client
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(fecho.Echo.$serviceName))
          ..from(Ref.child(v2EchoServer))
          ..to(Ref.child(v2EchoClient)));

        // Connect to the framework's EventStream.
        final eventStream = fcomponent.EventStreamProxy();
        await (services.Incoming.fromSvcPath()..connectToService(eventStream))
            .close();

        // Listen for "stopped" events.
        //
        // NOTE: This requires the test CML include a `use` for the subscribed
        // event type(s), for example:
        //
        // ```cml
        //   use: [
        //     {
        //         event_stream: [
        //             "started",
        //             "stopped",
        //         ],
        //         from: "parent",
        //     },
        //   ],
        // ```
        final completeWaitForStop = Completer<int>();
        unawaited(
            EventListener.callback(eventStream, (fcomponent.Event event) async {
          if (event.header?.eventType == fcomponent.EventType.stopped) {
            String? moniker = event.header?.moniker;
            int? stoppedStatus = event.payload?.stopped?.status;
            if (stoppedStatus != null &&
                (moniker?.endsWith('/$echoClientName') ?? false)) {
              // Since the EventListener is not closed at the end of this test,
              // it's possible another test could re-trigger the callback, so
              // ensure the Completer only completes once.
              if (!completeWaitForStop.isCompleted) {
                completeWaitForStop.complete(stoppedStatus);
              }
            }
          }
        }).listen());

        // Start the realm instance.
        realmInstance = await builder.build();

        // Wait for the client to stop, and check for a successful exit status.
        final stoppedStatus = await completeWaitForStop.future;
        expect(stoppedStatus, 0);
      } on Exception catch (err, stacktrace) {
        checkCommonExceptions(err, stacktrace);
        rethrow;
      } finally {
        if (realmInstance != null) {
          realmInstance.root.close();
        }
      }
    });

    test('packaged config', () async {
      RealmInstance? realmInstance;
      try {
        final builder = await RealmBuilder.create();

        const echoServerName = 'localEchoServer';
        const echoClientName = 'echoClient';

        final echoClientStructuredConfig = await builder.addChild(
          echoClientName,
          echoClientStructuredConfigUrl,
          ChildOptions()..eager(),
        );

        // NOTE: Important! This test updates the default configuration values
        // in the successful calls to `setConfigValue...()` below (after the
        // try/catch blocks).
        //
        // If this test was changed to run the EchoClient without first
        // replacing some configurations, the EchoClient's default configuration
        // values (as presently implemented) cause EchoClient to generate a
        // string that exceeds the `echoString()` FIDL API's [MAX_STRING_LENGTH]
        // (presently only 32, as declared in `echo.fidl`). This would cause the
        // client to crash with a FIDL error, and the request would never reach
        // the server.

        final echoRequestReceived = Completer<String?>();

        final localEchoServer = await builder.addLocalChild(
          echoServerName,
          onRun: (handles, onStop) async {
            LocalEcho(handles, echoRequestReceived: echoRequestReceived);

            // Keep the component alive until the test is complete
            await onStop.future;
          },
        );

        // load the packaged defaults for echo_bool and echo_num
        await builder.initMutableConfigFromPackage(echoClientStructuredConfig);

        // Route logging to children
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(flogger.LogSink.$serviceName))
          ..from(Ref.parent())
          ..to(Ref.child(localEchoServer))
          ..to(Ref.child(echoClientStructuredConfig)));

        // Route the echo service from server to client
        await builder.addRoute(Route()
          ..capability(ProtocolCapability(fecho.Echo.$serviceName))
          ..from(Ref.child(localEchoServer))
          ..to(Ref.child(echoClientStructuredConfig)));

        // The EchoClient at the referenced URL should be using this string:
        const echoClientStructuredConfigRequest =
            'Hello Fuchsia!, Hi, There, false, 100';

        // Start the realm instance.
        realmInstance = await builder.build();

        final requestedEchoString = await echoRequestReceived.future;

        expect(requestedEchoString, echoClientStructuredConfigRequest);
      } on Exception catch (err, stacktrace) {
        checkCommonExceptions(err, stacktrace);
        rethrow;
      } finally {
        if (realmInstance != null) {
          realmInstance.root.close();
        }
      }
    });

    group('local child tests', () {
      test('local server', () async {
        RealmInstance? realmInstance;
        try {
          final builder = await RealmBuilder.create();

          const echoServerName = 'localEchoServer';

          final localEchoServer = await builder.addLocalChild(
            echoServerName,
            onRun: (handles, onStop) async {
              LocalEcho(handles);
              // Keep the component alive until the test is complete
              await onStop.future;
            },
          );

          await builder.addRoute(Route()
            ..capability(ProtocolCapability(fecho.Echo.$serviceName))
            ..from(Ref.child(localEchoServer))
            ..to(Ref.parent()));

          realmInstance = await builder.build();

          final echo = realmInstance.root
              .connectToProtocolAtExposedDir(fecho.EchoProxy());
          const testString = 'ECHO...Echo...echo...(echo)...';

          final reply = await echo.echoString(testString);

          expect(testString, reply);
        } on Exception catch (err, stacktrace) {
          checkCommonExceptions(err, stacktrace);
          rethrow;
        } finally {
          if (realmInstance != null) {
            realmInstance.root.close();
          }
        }
      });
    });
  });
}

class LocalEcho extends fecho.Echo {
  final LocalComponentHandles handles;

  final Completer<String?>? echoRequestReceived;

  final echoBinding = fecho.EchoBinding();

  LocalEcho(this.handles, {this.echoRequestReceived}) {
    services.Outgoing()
      ..serve(
          fidl.InterfaceRequest<fio.Node>(handles.outgoingDir.passChannel()!))
      ..addPublicService(
        (fidl.InterfaceRequest<fecho.Echo> connector) {
          echoBinding.bind(this, connector);
        },
        fecho.Echo.$serviceName,
      );
  }

  @override
  Future<String?> echoString(String? str) async {
    if (echoRequestReceived != null) {
      echoRequestReceived!.complete(str);
    }
    return str;
  }
}
