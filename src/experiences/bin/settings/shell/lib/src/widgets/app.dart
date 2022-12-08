// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(http://fxbug.dev/107480): Resolve lint issues and reenable analysis for file
// ignore_for_file: deprecated_member_use

import 'dart:math';

import 'package:flutter/material.dart';
import 'package:flutter_mobx/flutter_mobx.dart';
import 'package:internationalization/strings.dart';
import 'package:mobx/mobx.dart';
import 'package:shell_settings/src/states/settings_state.dart';
import 'package:shell_settings/src/utils/themes.dart';
import 'package:shell_settings/src/widgets/channel_settings.dart';
import 'package:shell_settings/src/widgets/keyboard_settings.dart';
import 'package:shell_settings/src/widgets/timezone_settings.dart';

/// Defines a widget to display shell settings.
class App extends StatelessWidget {
  static const kWidth = 900.0;

  final SettingsState settingsState;

  const App(this.settingsState);

  @override
  Widget build(BuildContext context) {
    return RepaintBoundary(
      child: Container(
        child: Observer(builder: (_) {
          final state = settingsState;
          return Row(
            children: [
              SizedBox(
                width: 1,
                child: Container(
                  color: Theme.of(context).indicatorColor,
                ),
              ),
              SizedBox(
                width: kWidth,
                child: ListTileTheme(
                  iconColor: Theme.of(context).colorScheme.onSurface,
                  selectedColor: Theme.of(context).colorScheme.onPrimary,
                  child: Column(
                    children: [
                      AppBar(
                        elevation: 0,
                        shape:
                            Border.all(color: Theme.of(context).indicatorColor),
                        leading: Icon(Icons.settings),
                        title: Text(
                          Strings.settings,
                          style: Theme.of(context).textTheme.headline6,
                        ),
                        actions: [
                          // Date time.
                          Center(
                            child: Observer(builder: (context) {
                              return Text(
                                settingsState.dateTime,
                                style: Theme.of(context).textTheme.bodyText1,
                              );
                            }),
                          ),
                          SizedBox(width: 12),
                        ],
                      ),
                      Expanded(
                        child: _ListSettings(settingsState),
                      ),
                      SizedBox(
                        height: 1,
                        child: Container(
                          color: Theme.of(context).indicatorColor,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
              SizedBox(
                width: 1,
                child: Container(
                  color: Theme.of(context).indicatorColor,
                ),
              ),
              if (state.allSettingsPageVisible)
                Expanded(
                  child: Container(
                    color: AppTheme.darkTheme.bottomAppBarColor,
                  ),
                ),
              if (state.timezonesPageVisible)
                Expanded(
                  child: TimezoneSettings(
                    state: state,
                    onChange: state.updateTimezone,
                  ),
                ),
              if (state.channelPageVisible)
                Expanded(
                  child: ChannelSettings(
                    state: state,
                    onChange: state.setTargetChannel,
                  ),
                ),
              if (state.keyboardPageVisible)
                Expanded(
                  child: KeyboardSettings(
                      state: state, onChange: state.updateKeymap),
                ),
            ],
          );
        }),
      ),
    );
  }
}

class _ListSettings extends StatelessWidget {
  final SettingsState settingsState;

  const _ListSettings(this.settingsState);

  @override
  Widget build(BuildContext context) {
    return Container(
      color: AppTheme.darkTheme.bottomAppBarColor,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Expanded(
            child: ListView(
              children: [
                // IP Address
                Observer(builder: (_) {
                  return ListTile(
                    contentPadding: EdgeInsets.symmetric(horizontal: 24),
                    leading: Icon(Icons.network_ping),
                    title: Text(Strings.network),
                    trailing: Wrap(
                      alignment: WrapAlignment.end,
                      crossAxisAlignment: WrapCrossAlignment.end,
                      spacing: 8,
                      children: [
                        settingsState.networkAddresses.isEmpty
                            ? Text('--')
                            : settingsState.networkAddresses.length == 1
                                ? Text(
                                    settingsState.networkAddresses.first,
                                    maxLines: 1,
                                    style: TextStyle(
                                      overflow: TextOverflow.ellipsis,
                                    ),
                                  )
                                : Tooltip(
                                    message: settingsState.networkAddresses
                                        .join('\n'),
                                    child: Text(
                                      settingsState.networkAddresses.join('\n'),
                                      maxLines: 1,
                                      style: TextStyle(
                                        overflow: TextOverflow.ellipsis,
                                      ),
                                      textAlign: TextAlign.right,
                                    ),
                                  ),
                      ],
                    ),
                  );
                }),
                // Battery
                Observer(builder: (_) {
                  return ListTile(
                    contentPadding: EdgeInsets.symmetric(horizontal: 24),
                    leading: Icon(settingsState.powerIcon),
                    title: Text(Strings.power),
                    trailing: Wrap(
                      alignment: WrapAlignment.end,
                      crossAxisAlignment: WrapCrossAlignment.center,
                      spacing: 8,
                      children: [
                        if (settingsState.powerLevel != null) ...[
                          Text('${settingsState.powerLevel!.toInt()}%'),
                          SizedBox(width: 4),
                        ],
                      ],
                    ),
                  );
                }),
                // Channel
                ListTile(
                  enabled: true,
                  contentPadding: EdgeInsets.symmetric(horizontal: 24),
                  leading: Icon(Icons.cloud_download),
                  title: Row(
                    crossAxisAlignment: CrossAxisAlignment.center,
                    children: [
                      Text(Strings.channel),
                      SizedBox(width: 48),
                      Expanded(
                        child: Text(
                          settingsState.currentChannel,
                          overflow: TextOverflow.ellipsis,
                          textAlign: TextAlign.right,
                          maxLines: 1,
                        ),
                      ),
                    ],
                  ),
                  trailing: Icon(Icons.arrow_right),
                  onTap: settingsState.showChannelSettings,
                ),
                // Volume
                Observer(builder: (_) {
                  return ListTile(
                    enabled: true,
                    contentPadding: EdgeInsets.symmetric(horizontal: 24),
                    leading: Icon(settingsState.volumeIcon),
                    title: Row(
                      crossAxisAlignment: CrossAxisAlignment.center,
                      children: [
                        Text(Strings.volume),
                        Expanded(
                          child: Slider(
                            value: settingsState.volumeLevel ?? 1,
                            onChanged: settingsState.setVolumeLevel,
                          ),
                        ),
                      ],
                    ),
                    trailing: settingsState.volumeMuted == true
                        ? OutlinedButton(
                            style: SettingsButtonStyle.outlinedButton(
                                Theme.of(context)),
                            onPressed: () =>
                                settingsState.setVolumeMute(muted: false),
                            child: Text(Strings.unmute.toUpperCase()),
                          )
                        : OutlinedButton(
                            style: SettingsButtonStyle.outlinedButton(
                                Theme.of(context)),
                            onPressed: () =>
                                settingsState.setVolumeMute(muted: true),
                            child: Text(Strings.mute.toUpperCase()),
                          ),
                  );
                }),
                // Brightness
                Observer(builder: (_) {
                  return ListTile(
                    contentPadding: EdgeInsets.symmetric(horizontal: 24),
                    leading: Icon(settingsState.brightnessIcon),
                    title: Row(
                      crossAxisAlignment: CrossAxisAlignment.center,
                      children: [
                        Text(Strings.brightness),
                        Expanded(
                          child: Slider(
                            value:
                                max(settingsState.brightnessLevel ?? 1, 0.05),
                            onChanged: settingsState.setBrightnessLevel,
                            min: 0.05,
                          ),
                        ),
                      ],
                    ),
                    trailing: settingsState.brightnessAuto == true
                        ? Text(Strings.auto.toUpperCase())
                        : OutlinedButton(
                            style: SettingsButtonStyle.outlinedButton(
                                Theme.of(context)),
                            onPressed: settingsState.setBrightnessAuto,
                            child: Text(Strings.auto.toUpperCase()),
                          ),
                  );
                }),
                // Timezone
                ListTile(
                  contentPadding: EdgeInsets.symmetric(horizontal: 24),
                  leading: Icon(Icons.schedule),
                  title: Text(Strings.timezone),
                  trailing: Wrap(
                    alignment: WrapAlignment.end,
                    crossAxisAlignment: WrapCrossAlignment.center,
                    spacing: 8,
                    children: [
                      Text(
                        settingsState.selectedTimezone
                            // Remove '_' from city names.
                            .replaceAll('_', ' ')
                            .replaceAll('/', ' / '),
                        style: TextStyle(
                            color: Theme.of(context).colorScheme.secondary),
                      ),
                      Icon(Icons.arrow_right),
                    ],
                  ),
                  onTap: settingsState.showTimezoneSettings,
                ),
                // Keyboard Input
                ListTile(
                  contentPadding: EdgeInsets.symmetric(horizontal: 24),
                  leading: Icon(Icons.keyboard),
                  title: Text(Strings.keyboard),
                  trailing: Wrap(
                    alignment: WrapAlignment.end,
                    crossAxisAlignment: WrapCrossAlignment.center,
                    spacing: 8,
                    children: [
                      Text(settingsState.currentKeymap),
                      Icon(Icons.arrow_right),
                    ],
                  ),
                  onTap: settingsState.showKeyboardSettings,
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}
