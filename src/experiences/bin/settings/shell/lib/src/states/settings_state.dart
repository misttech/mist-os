// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'package:flutter/material.dart' hide Action;
import 'package:shell_settings/src/services/battery_watcher_service.dart';
import 'package:shell_settings/src/services/brightness_service.dart';
import 'package:shell_settings/src/services/channel_service.dart';
import 'package:shell_settings/src/services/datetime_service.dart';
import 'package:shell_settings/src/services/keyboard_service.dart';
import 'package:shell_settings/src/services/network_address_service.dart';
import 'package:shell_settings/src/services/task_service.dart';
import 'package:shell_settings/src/services/timezone_service.dart';
import 'package:shell_settings/src/services/volume_service.dart';
import 'package:shell_settings/src/services/wifi_service.dart';
import 'package:shell_settings/src/states/settings_state_impl.dart';
import 'package:shell_settings/src/widgets/setting_details.dart';

/// Defines the pages that have a [SettingDetails] widget.
enum SettingsPage {
  none,
  timezone,
  channel,
  keyboard,
  wifi,
}

/// Defines states for channel ota.
enum ChannelState {
  idle,
  checkingForUpdates,
  errorCheckingForUpdate,
  noUpdateAvailable,
  installationDeferredByPolicy,
  installingUpdate,
  waitingForReboot,
  installationError
}

/// Defines states for battery charge.
enum BatteryCharge {
  missing,
  charging,
  discharging,
  error,
}

/// Defines the state of the main settings overlay.
abstract class SettingsState implements TaskService {
  bool get allSettingsPageVisible;
  // Timezone
  bool get timezonesPageVisible;
  String get selectedTimezone;
  List<String> get timezones;
  // Datetime
  String get dateTime;
  // Brightness
  double? get brightnessLevel;
  bool? get brightnessAuto;
  IconData get brightnessIcon;
  // Channel
  bool get channelPageVisible;
  String get currentChannel;
  List<String> get availableChannels;
  String get targetChannel;
  ChannelState get channelState;
  bool? get optedIntoUpdates;
  double get systemUpdateProgress;
  // Volume
  IconData get volumeIcon;
  double? get volumeLevel;
  bool? get volumeMuted;
  // Keyboard
  bool get keyboardPageVisible;
  String get currentKeymap;
  List<String> get supportedKeymaps;
  // Battery
  BatteryCharge get batteryCharge;
  IconData get powerIcon;
  double? get powerLevel;
  // Network
  List<String> get networkAddresses;
  // Wifi
  bool get wifiPageVisible;
  int get wifiToggleMillisecondsPassed;
  List<NetworkInformation> get availableNetworks;
  NetworkInformation get targetNetwork;
  List<NetworkInformation> get savedNetworks;
  TextEditingController get networkPasswordTextController;
  String get currentNetwork;
  bool get clientConnectionsEnabled;
  bool get clientConnectionsMonitor;

  factory SettingsState.fromEnv() {
    // ignore: unnecessary_cast
    return SettingsStateImpl(
      timezoneService: TimezoneService(),
      dateTimeService: DateTimeService(),
      brightnessService: BrightnessService(),
      channelService: ChannelService(),
      volumeService: VolumeService(),
      keyboardService: KeyboardService(),
      batteryWatcherService: BatteryWatcherService(),
      networkService: NetworkAddressService(),
      wifiService: WiFiService(),
    ) as SettingsState;
  }

  void showAllSettings();
  // Timezone
  void updateTimezone(String tz);
  void showTimezoneSettings();
  // Brightness
  void setBrightnessLevel(double value);
  void setBrightnessAuto();
  // TODO(fxb/113485): add keyboard shortcuts for brightness
  void increaseBrightness();
  void decreaseBrightness();
  // Channel
  void showChannelSettings();
  void setTargetChannel(String channel);
  void checkForUpdates();
  // Volume
  void setVolumeLevel(double value);
  void setVolumeMute({bool muted});
  // TODO(fxb/113485): add keyboard shortcuts for volume
  void increaseVolume();
  void decreaseVolume();
  void toggleMute();
  // Keyboard
  void showKeyboardSettings();
  void updateKeymap(String id);
  // Wifi
  void showWiFiSettings();
  void connectToNetwork([String password]);
  void setTargetNetwork(NetworkInformation network);
  void clearTargetNetwork();
  void removeNetwork(NetworkInformation network);
  void setClientConnectionsEnabled({bool enabled});
}
