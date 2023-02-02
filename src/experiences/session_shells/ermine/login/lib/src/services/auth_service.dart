// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'dart:async';
import 'dart:io' as io;
import 'package:fidl/fidl.dart';
import 'package:fidl_fuchsia_identity_account/fidl_async.dart' as faccount;
import 'package:fidl_fuchsia_identity_authentication/fidl_async.dart';
import 'package:fidl_fuchsia_io/fidl_async.dart';
import 'package:fidl_fuchsia_recovery/fidl_async.dart';
import 'package:fuchsia_logger/logger.dart';
import 'package:fuchsia_services/services.dart';
import 'package:fuchsia_vfs/vfs.dart';
import 'package:internationalization/strings.dart';
import 'package:mobx/mobx.dart';
import 'package:zircon/zircon.dart';

const kDeprecatedAccountName = 'created_by_user';
const kSystemPickedAccountName = 'picked_by_system';
const kUserPickedAccountName = 'picked_by_user';
const kAccountDataDirectory = 'account_data';
const kAccountCacheDirectory = 'account_cache';
const kAccountTmpDirectory = 'account_tmp';
const kCacheSubdirectory = 'cache/';
const kIncomingTmpDirectory = '/tmp/';
const kTmpSubdirectory = 'account/';
const kDeprecatedAccountMgr =
    'fuchsia.identity.account.DeprecatedAccountManager';

enum AuthOp { enrollment, authentication }

const kUseNewAccountManagerMarkerFile = '/pkg/config/use_new_account_manager';

/// Defines a service that performs authentication tasks like:
/// - create an account with password
/// - login to an account with password
/// - logout from an account
///
/// Note:
/// - It always picks the first account for login and logout.
/// - Creating an account, when an account already exists, is an undefined
///   behavior. The client of the service should ensure to not call account
///   creation in this case.
class AuthService {
  late final PseudoDir hostedDirectories;

  final _accountManager = faccount.AccountManagerProxy();
  faccount.AccountProxy? _account;
  int? _accountId;
  final _ready = false.asObservable();

  /// Set to true if successfully authenticated, false otherwise or post logout.
  bool authenticated = false;

  bool useNewAccountManager = false;

  AuthService() {
    useNewAccountManager =
        io.File(kUseNewAccountManagerMarkerFile).existsSync();

    if (useNewAccountManager) {
      Incoming.fromSvcPath().connectToService(_accountManager);
    } else {
      Incoming.fromSvcPath()
          .connectToService(_accountManager, name: kDeprecatedAccountMgr);
    }
  }

  void dispose() {
    _accountManager.ctrl.close();
  }

  /// Load existing accounts from [AccountManager].
  void loadAccounts() async {
    try {
      final ids = await _accountManager.getAccountIds();

      // TODO(http://fxb/85576): Remove once login and OOBE are mandatory.
      // Remove any accounts created with a deprecated name or from an auth
      // mode that does not match the current build configuration.
      for (var id in ids) {
        final metadata = await _accountManager.getAccountMetadata(id);

        if (metadata.name != null &&
            _shouldRemoveAccountWithName(metadata.name!)) {
          try {
            await _accountManager.removeAccount(id);
            ids.remove(id);
            log.info('Removed account: $id with name: ${metadata.name}');
            // ignore: avoid_catches_without_on_clauses
          } catch (e) {
            // We can only log and continue.
            log.shout('Failed during deprecated account removal: $e');
          }
        }
      }
      if (ids.length > 1) {
        log.shout(
            'Multiple (${ids.length}) accounts found, will use the first.');
      }

      if (ids.isNotEmpty) {
        _accountId = ids.first;
      }
      runInAction(() => _ready.value = true);
      // ignore: avoid_catches_without_on_clauses
    } catch (e) {
      log.shout('Failed during deprecated account removal: $e');
    }
  }

  bool _shouldRemoveAccountWithName(String name) {
    return name == kDeprecatedAccountName || name == kSystemPickedAccountName;
  }

  /// Calls [FactoryReset] service to factory data reset the device.
  void factoryReset() {
    final proxy = FactoryResetProxy();
    Incoming.fromSvcPath().connectToService(proxy);
    proxy
        .reset()
        .then((status) => log.info('Requested factory reset.'))
        .catchError((e) => log.shout('Failed to factory reset device: $e'));
  }

  /// Returns [true] after [_accountManager.getAccountIds()] completes.
  bool get ready => _ready.value;

  /// Returns [true] if no accounts exists on device.
  bool get hasAccount {
    assert(ready, 'Called before list of accounts could be retrieved.');
    return _accountId != null;
  }

  /// Returns the first logged in account id.
  /// [loadAccounts] should have been called first.
  int get accountId {
    assert(hasAccount);
    return _accountId!;
  }

  String errorFromException(Object e, AuthOp op) {
    if (e is MethodException) {
      switch (e.value as faccount.Error) {
        case faccount.Error.unsupportedOperation:
          // TODO(sanjayc): Create Strings.unsupportedOperation.
          return 'Unsupported Operation';
        case faccount.Error.failedAuthentication:
          return Strings.accountPasswordFailedAuthentication;
        case faccount.Error.notFound:
          switch (op) {
            case AuthOp.authentication:
              return Strings.accountNotFound;
            case AuthOp.enrollment:
              return Strings.accountPartitionNotFound;
          }
      }
    }
    return e.toString();
  }

  /// Creates an account with password and sets up the account data directory.
  Future<void> createAccountWithPassword(String password) async {
    assert(_account == null, 'An account already exists.');
    if (_account != null && _account!.ctrl.isBound) {
      // ignore: unawaited_futures
      _account!.storageLock().catchError((_) {});
      _account!.ctrl.close();
    }

    final metadata = faccount.AccountMetadata(
        name: password.isEmpty
            ? kSystemPickedAccountName
            : kUserPickedAccountName);

    if (useNewAccountManager) {
      try {
        final passwordInteractionFlow =
            await _getPasswordInteractionFlowForEnroll(metadata);

        final id = await passwordInteractionFlow.setPassword(password);
        clearPasswordInteractionFlow();

        _account = faccount.AccountProxy();
        await _accountManager
            .getAccount(faccount.AccountManagerGetAccountRequest(
          id: id,
          account: _account!.ctrl.request(),
        ));
      } on PasswordInteractionException catch (_) {
        // TODO(fxb/119277): Update to support "waitingForPassword" timer based
        // back off for next password attempt.
        // Call PasswordInteraction.watchState again to move the interaction
        // state machine from error state to waiting to set password state.
        final passwordInteractionFlow =
            await _getPasswordInteractionFlowForEnroll(metadata);
        final result =
            await passwordInteractionFlow.passwordInteraction.watchState();

        assert(result.$tag ==
                PasswordInteractionWatchStateResponseTag.waiting &&
            result.waiting
                    ?.contains(PasswordCondition.withSetPassword(Empty())) ==
                true);
        rethrow;
      }
    } else {
      _account = faccount.AccountProxy();
      await _accountManager.deprecatedProvisionNewAccount(
        password,
        metadata,
        _account!.ctrl.request(),
      );
    }

    final ids = await _accountManager.getAccountIds();
    _accountId = ids.first;
    log.info('Account creation succeeded. $_accountId');

    await _publishAccountDirectory(_account!);

    authenticated = true;
  }

  /// Logs in to the first account with [password] and sets up the account data
  /// directory.
  Future<void> loginWithPassword(String password) async {
    assert(_accountId != null, 'No account exist to login to.');
    if (_account != null && _account!.ctrl.isBound) {
      // ignore: unawaited_futures
      _account!.storageLock().catchError((_) {});
      _account!.ctrl.close();
    }

    if (useNewAccountManager) {
      try {
        final passwordInteractionFlow =
            await _getPasswordInteractionFlowForAuth(
                _accountId!.toUnsigned(64));
        await passwordInteractionFlow.setPassword(password);
        clearPasswordInteractionFlow();
        // ignore: avoid_catches_without_on_clauses
      } catch (e) {
        // Clear password interaction flow on invalid password.
        if (e is MethodException &&
            e.value == faccount.Error.failedAuthentication) {
          clearPasswordInteractionFlow();
        }
        rethrow;
      }
    } else {
      _account = faccount.AccountProxy();
      await _accountManager.deprecatedGetAccount(
        _accountId!.toUnsigned(64),
        password,
        _account!.ctrl.request(),
      );
    }
    log.info('Login to first account on device succeeded.');

    await _publishAccountDirectory(_account!);

    authenticated = true;
  }

  /// Logs out of an account by locking storage for it and deleting the
  /// associated tmp directory.
  Future<void> logout() async {
    assert(_account != null, 'No account exists to logout from.');

    log.info('Locking storage for account.');
    await _account!.storageLock();

    authenticated = false;

    // We expect the tmp subdirectory should exist by logout, but if it
    // doesn't then we can just continue without attempting deletion.
    if (io.Directory('$kIncomingTmpDirectory$kTmpSubdirectory').existsSync()) {
      log.info('Deleting tmp directory for account.');
      await io.Directory('$kIncomingTmpDirectory$kTmpSubdirectory')
          .delete(recursive: true);
    }
  }

  /// Publishes all flavors of storage directory for the supplied account.
  Future<void> _publishAccountDirectory(faccount.Account account) async {
    // Get the data directory for the account.
    log.info('Getting data directory for account.');
    final dataDirChannel = ChannelPair();
    await account.getDataDirectory(InterfaceRequest(dataDirChannel.second));

    // Open or create a subdirectory for the cache storage capability.
    log.info('Opening cache directory for account.');
    final dataDir = RemoteDir(dataDirChannel.first!);
    final cacheSubdirChannel = ChannelPair();
    dataDir.open(
        OpenFlags.rightReadable |
            OpenFlags.rightWritable |
            OpenFlags.create |
            OpenFlags.directory,
        ModeType.$none,
        kCacheSubdirectory,
        InterfaceRequest(cacheSubdirChannel.second));

    // Create a directory for the tmp storage capability.
    log.info('Creating tmp directory for account.');
    final tmpDir = RemoteDir(Channel.fromFile(kIncomingTmpDirectory));
    final tmpSubdirChannel = ChannelPair();
    tmpDir.open(
        OpenFlags.rightReadable |
            OpenFlags.rightWritable |
            OpenFlags.create |
            OpenFlags.directory,
        ModeType.$none,
        kTmpSubdirectory,
        InterfaceRequest(tmpSubdirChannel.second));

    // Host all directories.
    hostedDirectories
      ..removeNode(kAccountDataDirectory)
      ..addNode(kAccountDataDirectory, dataDir)
      ..removeNode(kAccountCacheDirectory)
      ..addNode(kAccountCacheDirectory, RemoteDir(cacheSubdirChannel.first!))
      ..removeNode(kAccountTmpDirectory)
      ..addNode(kAccountTmpDirectory, RemoteDir(tmpSubdirChannel.first!));

    log.info('Data, cache, and tmp directories for account published.');
  }

  _PasswordInteractionFlow? _passwordInteractionFlow;

  void clearPasswordInteractionFlow() => _passwordInteractionFlow = null;

  Future<_PasswordInteractionFlow> _getPasswordInteractionFlowForAuth(
      int accountId) async {
    return _passwordInteractionFlow ??= await () async {
      _account = faccount.AccountProxy();
      return _PasswordInteractionFlow.forAuthentication(
          _accountManager, accountId, _account!);
    }();
  }

  Future<_PasswordInteractionFlow> _getPasswordInteractionFlowForEnroll(
      faccount.AccountMetadata metadata) async {
    return _passwordInteractionFlow ??= await () async {
      return _PasswordInteractionFlow.forEnrollment(_accountManager, metadata);
    }();
  }
}

// Defines set password flow used during enrollment and authentication.
class _PasswordInteractionFlow {
  // The future that completes when [AccountManager.getAccount] or
  // [AccountManager.provisionNewAccount] succeeds.
  final Future getAccountOrProvisionNewAccountFuture;

  // Holds the connection to [Interaction] proxy and used to wait for the
  // underlying channel to close signifying success.
  final InteractionProxy interaction;

  // Holds the connection to [PasswordInteraction] proxy and used to wait for
  // the underlying channel to close signifying success.
  final PasswordInteractionProxy passwordInteraction;

  // Private constructor to hold state during password interaction flow.
  _PasswordInteractionFlow._(this.getAccountOrProvisionNewAccountFuture,
      this.interaction, this.passwordInteraction);

  static Future<_PasswordInteractionFlow> forAuthentication(
      faccount.AccountManagerProxy accountManager,
      int id,
      faccount.AccountProxy account) async {
    final interaction = InteractionProxy();
    final passwordInteraction = PasswordInteractionProxy();
    final getAccountOrProvisionNewAccountCompleter = Completer();

    // ignore: unawaited_futures
    accountManager
        .getAccount(faccount.AccountManagerGetAccountRequest(
          id: id,
          interaction: InterfaceRequest<Interaction>(
              interaction.ctrl.request().passChannel()),
          account: account.ctrl.request(),
        ))
        .then(getAccountOrProvisionNewAccountCompleter.complete)
        .catchError(getAccountOrProvisionNewAccountCompleter.completeError);

    // ignore: unawaited_futures
    interaction
        .startPassword(
            InterfaceRequest(passwordInteraction.ctrl.request().passChannel()),
            Mode.authenticate)
        .catchError(getAccountOrProvisionNewAccountCompleter.completeError);

    // Ensure watchState is waiting for setPassword.
    final response = await passwordInteraction.watchState();

    if (response.$tag != PasswordInteractionWatchStateResponseTag.waiting ||
        !response.waiting!
            .contains(PasswordCondition.withSetPassword(Empty()))) {
      throw PasswordInteractionException(response);
    }

    return _PasswordInteractionFlow._(
        getAccountOrProvisionNewAccountCompleter.future,
        interaction,
        passwordInteraction);
  }

  static Future<_PasswordInteractionFlow> forEnrollment(
      faccount.AccountManagerProxy accountManager,
      faccount.AccountMetadata metadata) async {
    final interaction = InteractionProxy();
    final passwordInteraction = PasswordInteractionProxy();
    final getAccountOrProvisionNewAccountCompleter = Completer();

    // ignore: unawaited_futures
    accountManager
        .provisionNewAccount(faccount.AccountManagerProvisionNewAccountRequest(
          lifetime: faccount.Lifetime.persistent,
          metadata: metadata,
          interaction: InterfaceRequest<Interaction>(
              interaction.ctrl.request().passChannel()),
        ))
        .then(getAccountOrProvisionNewAccountCompleter.complete)
        .catchError(getAccountOrProvisionNewAccountCompleter.completeError);

    // ignore: unawaited_futures
    interaction
        .startPassword(
            InterfaceRequest(passwordInteraction.ctrl.request().passChannel()),
            Mode.enroll)
        .catchError(getAccountOrProvisionNewAccountCompleter.completeError);

    // Ensure watchState is waiting for setPassword.
    final response = await passwordInteraction.watchState();
    if (response.$tag != PasswordInteractionWatchStateResponseTag.waiting ||
        !response.waiting!
            .contains(PasswordCondition.withSetPassword(Empty()))) {
      throw PasswordInteractionException(response);
    }

    return _PasswordInteractionFlow._(
        getAccountOrProvisionNewAccountCompleter.future,
        interaction,
        passwordInteraction);
  }

  // Calls setPassword and watchState on passwordInteraction channel without
  // blocking. It responds to change in state asynchronously, converting any
  // state change as an exception, which is reported from a blocking wait on
  // a list of channel closure futures and watchState future.
  Future<dynamic> setPassword(String password) async {
    // Set the password without blocking. This will allow us to call watchSate
    // right after.
    // ignore: unawaited_futures
    passwordInteraction.setPassword(password).catchError((_) {});

    // Watch for state change without blocking and report any state as an
    // exception to allow updating the UX accordingly.
    final watchStateCompleter = Completer();
    // ignore: unawaited_futures
    passwordInteraction
        .watchState()
        .then((response) => watchStateCompleter
            .completeError(PasswordInteractionException(response)))
        .catchError(watchStateCompleter.complete);

    // Wait on futures from the auth/enroll method and channel closures on
    // passwordInteraction and interaction channels. These channels are closed
    // upon successful auth/enroll. The list also includes the future from the
    // watchState completer, which throws an exception upon state change.
    final result = await Future.wait(
      [
        getAccountOrProvisionNewAccountFuture,
        passwordInteraction.ctrl.whenClosed,
        interaction.ctrl.whenClosed,
        watchStateCompleter.future,
      ],
      eagerError: true,
    );

    return result.first;
  }
}

// A custom exception to allow clients of the AuthService to handle change in
// watchState response.
class PasswordInteractionException implements Exception {
  final PasswordInteractionWatchStateResponse response;
  late final bool shouldWait;
  late final String message;

  PasswordInteractionException(this.response) {
    _processResponse();
  }

  @override
  String toString() => message;

  void _processResponse() {
    switch (response.$tag) {
      case PasswordInteractionWatchStateResponseTag.waiting:
        message = 'Please wait';
        shouldWait = true;
        for (final condition in response.waiting!) {
          if (condition.$tag == PasswordConditionTag.waitUntil) {
            // TODO(sanjayc): Also report the duration for a countdown UX.
            message = 'Please wait';
            shouldWait = true;
            break;
          }
        }
        break;
      case PasswordInteractionWatchStateResponseTag.verifying:
        shouldWait = true;
        message = 'Verifying password';
        break;
      case PasswordInteractionWatchStateResponseTag.error:
        shouldWait = false;
        switch (response.error?.$tag) {
          case PasswordErrorTag.tooShort:
            message = 'Password too short';
            break;
          case PasswordErrorTag.tooWeak:
            message = 'Password too weak';
            break;
          case PasswordErrorTag.incorrect:
            message = 'Password incorrect';
            break;
          case PasswordErrorTag.mustWait:
            message = 'Must wait';
            break;
          case PasswordErrorTag.notWaitingForPassword:
            message = 'Not waiting for password';
            break;
          case PasswordErrorTag.$unknown:
          default:
            message = response.error.toString();
            break;
        }
        break;
    }
  }
}
