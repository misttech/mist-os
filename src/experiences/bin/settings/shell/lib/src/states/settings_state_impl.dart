// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(http://fxbug.dev/107480): Resolve lint issues and reenable analysis for file
// ignore_for_file: cascade_invocations

import 'dart:io';

import 'package:ermine_utils/ermine_utils.dart';
import 'package:flutter/material.dart' hide Action;
import 'package:flutter/services.dart';
import 'package:intl/intl.dart';
import 'package:mobx/mobx.dart';
import 'package:shell_settings/src/services/battery_watcher_service.dart';
import 'package:shell_settings/src/services/brightness_service.dart';
import 'package:shell_settings/src/services/channel_service.dart';
import 'package:shell_settings/src/services/datetime_service.dart';
import 'package:shell_settings/src/services/keyboard_service.dart';
import 'package:shell_settings/src/services/memory_watcher_service.dart';
import 'package:shell_settings/src/services/network_address_service.dart';
import 'package:shell_settings/src/services/task_service.dart';
import 'package:shell_settings/src/services/timezone_service.dart';
import 'package:shell_settings/src/services/volume_service.dart';
import 'package:shell_settings/src/services/wifi_service.dart';
import 'package:shell_settings/src/states/settings_state.dart';

/// Defines the implementation of [SettingsState].
class SettingsStateImpl with Disposable implements SettingsState, TaskService {
  static const kTimezonesFile = '/pkg/data/tz_ids.txt';

  // All
  final settingsPage = SettingsPage.none.asObservable();

  @override
  bool get allSettingsPageVisible => _allSettingsPageVisible.value;
  late final _allSettingsPageVisible =
      (() => settingsPage.value == SettingsPage.none).asComputed();

  // Timezone
  @override
  bool get timezonesPageVisible => _timezonesPageVisible.value;
  late final _timezonesPageVisible =
      (() => settingsPage.value == SettingsPage.timezone).asComputed();

  @override
  String get selectedTimezone => _selectedTimezone.value;
  set selectedTimezone(String value) => _selectedTimezone.value = value;
  final Observable<String> _selectedTimezone;

  final List<String> _timezones;

  @override
  List<String> get timezones {
    // Move the selected timezone to the top.
    return [selectedTimezone]
      ..addAll(_timezones.where((zone) => zone != selectedTimezone));
  }

  // Datetime
  @override
  String get dateTime => _dateTime.value;
  late final ObservableValue<String> _dateTime = (() =>
      // Ex: Mon, Jun 7 2:25 AM
      DateFormat.MMMEd().add_jm().format(dateTimeNow.value)).asComputed();

  // Brightness
  @override
  double? get brightnessLevel => _brightnessLevel.value;
  set brightnessLevel(double? value) => _brightnessLevel.value = value;
  final Observable<double?> _brightnessLevel = Observable<double?>(null);

  @override
  bool? get brightnessAuto => _brightnessAuto.value;
  set brightnessAuto(bool? value) => _brightnessAuto.value = value;
  final Observable<bool?> _brightnessAuto = Observable<bool?>(null);

  @override
  IconData get brightnessIcon => _brightnessIcon.value;
  set brightnessIcon(IconData value) => _brightnessIcon.value = value;
  final Observable<IconData> _brightnessIcon =
      Icons.brightness_auto.asObservable();

  // Channel
  @override
  bool get channelPageVisible => _channelPageVisible.value;
  late final _channelPageVisible =
      (() => settingsPage.value == SettingsPage.channel).asComputed();

  @override
  String get currentChannel => _currentChannel.value;
  set currentChannel(String value) => _currentChannel.value = value;
  final Observable<String> _currentChannel = Observable<String>('');

  @override
  final List<String> availableChannels = ObservableList<String>();

  @override
  String get targetChannel => _targetChannel.value;
  set targetChannel(String value) => _targetChannel.value = value;
  final Observable<String> _targetChannel = Observable<String>('');

  @override
  ChannelState get channelState => _channelState.value;
  set channelState(ChannelState value) => _channelState.value = value;
  final _channelState = Observable<ChannelState>(ChannelState.idle);

  @override
  bool? get optedIntoUpdates => _optedIntoUpdates.value;
  set optedIntoUpdates(bool? value) => _optedIntoUpdates.value = value;
  final Observable<bool?> _optedIntoUpdates = Observable<bool?>(null);

  @override
  double get systemUpdateProgress => _systemUpdateProgress.value;
  set systemUpdateProgress(double value) => _systemUpdateProgress.value = value;
  final _systemUpdateProgress = Observable<double>(0);

  // Volume
  @override
  IconData get volumeIcon => _volumeIcon.value;
  set volumeIcon(IconData value) => _volumeIcon.value = value;
  final Observable<IconData> _volumeIcon = Icons.volume_up.asObservable();

  @override
  double? get volumeLevel => _volumeLevel.value;
  set volumeLevel(double? value) => _volumeLevel.value = value;
  final Observable<double?> _volumeLevel = Observable<double?>(null);

  @override
  bool? get volumeMuted => _volumeMuted.value;
  set volumeMuted(bool? value) => _volumeMuted.value = value;
  final Observable<bool?> _volumeMuted = Observable<bool?>(null);

  // Keyboard
  @override
  bool get keyboardPageVisible => _keyboardPageVisible.value;
  late final _keyboardPageVisible =
      (() => settingsPage.value == SettingsPage.keyboard).asComputed();

  @override
  String get currentKeymap => _currentKeymap.value;
  set currentKeymap(String value) => _currentKeymap.value = value;
  final Observable<String> _currentKeymap = Observable<String>('');

  @override
  final List<String> supportedKeymaps = ObservableList<String>();

  // Battery
  @override
  BatteryCharge get batteryCharge => _batteryCharge.value;
  final _batteryCharge = Observable<BatteryCharge>(BatteryCharge.charging);

  @override
  IconData get powerIcon => _powerIcon.value;
  set powerIcon(IconData value) => _powerIcon.value = value;
  final Observable<IconData> _powerIcon = Icons.battery_unknown.asObservable();

  @override
  double? get powerLevel => _powerLevel.value;
  set powerLevel(double? value) => _powerLevel.value = value;
  final Observable<double?> _powerLevel = Observable<double?>(null);

  // Network
  @override
  final networkAddresses = ObservableList<String>();

  // Wifi
  @override
  bool get wifiPageVisible => _wifiPageVisible.value;
  late final _wifiPageVisible =
      (() => settingsPage.value == SettingsPage.wifi).asComputed();

  @override
  NetworkInformation get targetNetwork => _targetNetwork.value;
  set targetNetwork(NetworkInformation value) => _targetNetwork.value = value;
  final Observable<NetworkInformation> _targetNetwork =
      Observable<NetworkInformation>(NetworkInformation());

  @override
  TextEditingController get networkPasswordTextController =>
      _networkPasswordTextController;
  final TextEditingController _networkPasswordTextController =
      TextEditingController();

  @override
  final List<NetworkInformation> availableNetworks =
      ObservableList<NetworkInformation>();

  @override
  final List<NetworkInformation> savedNetworks =
      ObservableList<NetworkInformation>();

  @override
  String get currentNetwork => _currentNetwork.value;
  set currentNetwork(String value) => _currentNetwork.value = value;
  final Observable<String> _currentNetwork = ''.asObservable();

  @override
  int get wifiToggleMillisecondsPassed => _wifiToggleMillisecondsPassed.value;
  set wifiToggleMillisecondsPassed(int value) =>
      _wifiToggleMillisecondsPassed.value = value;
  final Observable<int> _wifiToggleMillisecondsPassed = Observable<int>(0);

  @override
  bool get clientConnectionsEnabled => _clientConnectionsEnabled.value;
  set clientConnectionsEnabled(bool value) =>
      _clientConnectionsEnabled.value = value;
  final Observable<bool> _clientConnectionsEnabled = Observable<bool>(true);

  @override
  bool get clientConnectionsMonitor => _clientConnectionsMonitor.value;
  set clientConnectionsMonitor(bool value) =>
      _clientConnectionsMonitor.value = value;
  final Observable<bool> _clientConnectionsMonitor = Observable<bool>(true);

  // Memory
  @override
  String get memUsed => _memUsed.value;
  set memUsed(String value) => _memUsed.value = value;
  final Observable<String> _memUsed = '--'.asObservable();

  @override
  String get memTotal => _memTotal.value;
  set memTotal(String value) => _memTotal.value = value;
  final Observable<String> _memTotal = '--'.asObservable();

  @override
  double? get memPercentUsed => _memPercentUsed.value;
  set memPercentUsed(double? value) => _memPercentUsed.value = value;
  final Observable<double?> _memPercentUsed = Observable<double?>(null);

  // Services
  final BrightnessService brightnessService;
  final DateTimeService dateTimeService;
  final TimezoneService timezoneService;
  final ChannelService channelService;
  final VolumeService volumeService;
  final KeyboardService keyboardService;
  final BatteryWatcherService batteryWatcherService;
  final NetworkAddressService networkService;
  final WiFiService wifiService;
  final MemoryWatcherService memoryWatcherService;

  // Constructor
  SettingsStateImpl({
    required this.dateTimeService,
    required this.timezoneService,
    required this.brightnessService,
    required this.channelService,
    required this.volumeService,
    required this.keyboardService,
    required this.batteryWatcherService,
    required this.networkService,
    required this.wifiService,
    required this.memoryWatcherService,
  })  : _timezones = _loadTimezones(),
        _selectedTimezone = timezoneService.timezone.asObservable() {
    dateTimeService.onChanged = updateDateTime;
    timezoneService.onChanged =
        (timezone) => runInAction(() => selectedTimezone = timezone);
    brightnessService.onChanged = () {
      runInAction(() {
        brightnessLevel = brightnessService.brightness;
        brightnessAuto = brightnessService.auto;
        brightnessIcon = brightnessService.icon;
      });
    };
    channelService.onChanged = () {
      runInAction(() {
        optedIntoUpdates = channelService.optedIntoUpdates;
        currentChannel = channelService.currentChannel;
        systemUpdateProgress = channelService.updateProgress;
        List<String> channels;
        // FIDL messages are unmodifiable so we need to copy the list of
        // channels here.
        channels = channelService.channels.toList();
        // Sort channels for readability
        channels.sort();
        availableChannels
          ..clear()
          ..addAll(channels);
        targetChannel = channelService.targetChannel;
        // Monitor state of update
        if (channelService.checkingForUpdates) {
          channelState = ChannelState.checkingForUpdates;
        } else if (channelService.errorCheckingForUpdate) {
          channelState = ChannelState.errorCheckingForUpdate;
        } else if (channelService.noUpdateAvailable) {
          channelState = ChannelState.noUpdateAvailable;
        } else if (channelService.installationDeferredByPolicy) {
          channelState = ChannelState.installationDeferredByPolicy;
        } else if (channelService.installingUpdate) {
          channelState = ChannelState.installingUpdate;
        } else if (channelService.waitingForReboot) {
          channelState = ChannelState.waitingForReboot;
        } else if (channelService.installationError) {
          channelState = ChannelState.installationError;
        }
      });
    };
    volumeService.onChanged = () {
      runInAction(() {
        volumeLevel = volumeService.volume;
        volumeIcon = volumeService.icon;
        volumeMuted = volumeService.muted;
      });
    };
    keyboardService.onChanged = () {
      runInAction(() {
        currentKeymap = keyboardService.currentKeymap;
        supportedKeymaps
          ..clear()
          ..addAll(keyboardService.supportedKeymaps);
      });
    };
    batteryWatcherService.onChanged = () {
      runInAction(() {
        powerIcon = batteryWatcherService.icon;
        powerLevel = batteryWatcherService.levelPercent;
        if (powerLevel != null) {
          final level = powerLevel!.toInt();

          if (level == 2 && batteryWatcherService.isLevelDropping) {
            // TODO(fxb/113485): show alert if battery level low
          }
        }
      });
    };
    networkService.onChanged = () => NetworkInterface.list().then((interfaces) {
          // Gather all addresses from all interfaces and sort them such that
          // IPv4 addresses come before IPv6.
          final addresses = interfaces
              .expand((interface) => interface.addresses)
              .toList(growable: false)
            ..sort((addr1, addr2) =>
                addr1.type == InternetAddressType.IPv4 ? -1 : 0);

          runInAction(() => networkAddresses
            ..clear()
            ..addAll(addresses.map((address) => address.address)));
        });
    wifiService.onChanged = () {
      runInAction(() {
        // TODO(fxb/79885): remove network names with non-ASCII characters
        availableNetworks
          ..clear()
          ..addAll(wifiService.scannedNetworks)
          ..removeWhere((network) => network.name.isEmpty);
        targetNetwork = wifiService.targetNetwork;
        // TODO(fxb/79885): remove saved networks from available networks
        // by ssid & security type
        savedNetworks
          ..clear()
          ..addAll(wifiService.savedNetworks)
          ..removeWhere((network) => network.name.isEmpty);
        currentNetwork = wifiService.currentNetwork;
        clientConnectionsEnabled = wifiService.clientConnectionsEnabled;
        clientConnectionsMonitor = wifiService.connectionsEnabled;
        wifiToggleMillisecondsPassed = wifiService.toggleMillisecondsPassed;
      });
    };
    memoryWatcherService.onChanged = () {
      runInAction(() {
        memUsed = '${memoryWatcherService.memUsed!.toStringAsPrecision(2)}GB';
        memTotal = '${memoryWatcherService.memTotal!.toStringAsPrecision(2)}GB';
        memPercentUsed =
            memoryWatcherService.memUsed! / memoryWatcherService.memTotal!;
      });
    };

    // We cannot load MaterialIcons font file from pubspec.yaml. So load it
    // explicitly.
    File file = File('/pkg/data/MaterialIcons-Regular.otf');
    if (file.existsSync()) {
      FontLoader('MaterialIcons')
        ..addFont(() async {
          return file.readAsBytesSync().buffer.asByteData();
        }())
        ..load();
    }
  }

  // Task service
  @override
  Future<void> start() async {
    await Future.wait([
      dateTimeService.start(),
      timezoneService.start(),
      brightnessService.start(),
      channelService.start(),
      volumeService.start(),
      keyboardService.start(),
      batteryWatcherService.start(),
      networkService.start(),
      wifiService.start(),
      memoryWatcherService.start(),
    ]);
  }

  @override
  Future<void> stop() async {
    showAllSettings();
    await dateTimeService.stop();
    await timezoneService.stop();
    await brightnessService.stop();
    await channelService.stop();
    await volumeService.stop();
    await keyboardService.stop();
    await batteryWatcherService.stop();
    await networkService.stop();
    await wifiService.stop();
    await memoryWatcherService.stop();
    _dateTimeNow = null;
  }

  @override
  void dispose() {
    super.dispose();
    dateTimeService.dispose();
    timezoneService.dispose();
    brightnessService.dispose();
    channelService.dispose();
    volumeService.dispose();
    keyboardService.dispose();
    batteryWatcherService.dispose();
    networkService.dispose();
    wifiService.dispose();
    memoryWatcherService.dispose();
  }

  // All
  @override
  void showAllSettings() =>
      runInAction(() => settingsPage.value = SettingsPage.none);

  // Timezone
  @override
  void updateTimezone(String timezone) => runInAction(() {
        selectedTimezone = timezone;
        timezoneService.timezone = timezone;
        settingsPage.value = SettingsPage.none;
      });

  @override
  void showTimezoneSettings() =>
      runInAction(() => settingsPage.value = SettingsPage.timezone);

  static List<String> _loadTimezones() {
    return File(kTimezonesFile).readAsLinesSync();
  }

  // Datetime
  Observable<DateTime>? _dateTimeNow;
  Observable<DateTime> get dateTimeNow =>
      _dateTimeNow ??= DateTime.now().asObservable();

  late final Action updateDateTime = () {
    dateTimeNow.value = DateTime.now();
  }.asAction();

  // Brightness
  @override
  void setBrightnessLevel(double value) =>
      runInAction(() => brightnessService.brightness = value);

  @override
  void increaseBrightness() =>
      runInAction(brightnessService.increaseBrightness);

  @override
  void decreaseBrightness() =>
      runInAction(brightnessService.decreaseBrightness);

  @override
  void setBrightnessAuto() => runInAction(() => brightnessService.auto = true);

  // Channel
  @override
  void showChannelSettings() =>
      runInAction(() => settingsPage.value = SettingsPage.channel);

  @override
  void setTargetChannel(String value) =>
      runInAction(() => channelService.targetChannel = value);

  @override
  void checkForUpdates() => runInAction(channelService.checkForUpdates);

  // Volume
  @override
  void setVolumeLevel(double value) =>
      runInAction(() => volumeService.volume = value);

  @override
  void setVolumeMute({bool muted = false}) =>
      runInAction(() => volumeService.muted = muted);

  @override
  void increaseVolume() => runInAction(volumeService.increaseVolume);

  @override
  void decreaseVolume() => runInAction(volumeService.decreaseVolume);

  @override
  void toggleMute() => runInAction(volumeService.toggleMute);

  // Keyboard
  @override
  void updateKeymap(String id) => runInAction(() {
        currentKeymap = id;
        keyboardService.currentKeymap = id;
        settingsPage.value = SettingsPage.none;
      });

  @override
  void showKeyboardSettings() =>
      runInAction(() => settingsPage.value = SettingsPage.keyboard);

  // Wifi
  @override
  void showWiFiSettings() =>
      runInAction(() => settingsPage.value = SettingsPage.wifi);

  @override
  void connectToNetwork([String password = '']) =>
      runInAction(() => wifiService.connectToNetwork(password));

  @override
  void setTargetNetwork(NetworkInformation network) =>
      runInAction(() => wifiService.targetNetwork = network);

  @override
  void clearTargetNetwork() =>
      runInAction(() => wifiService.targetNetwork = NetworkInformation());

  @override
  void removeNetwork(NetworkInformation network) =>
      runInAction(() => wifiService.remove(network));

  @override
  void setClientConnectionsEnabled({bool enabled = true}) =>
      runInAction(() => wifiService.clientConnectionsEnabled = enabled);
}
