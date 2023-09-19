#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Wlan policy affordance implementation using Fuchsia-Controller."""

from honeydew.interfaces.affordances.wlan import wlan_policy
from honeydew.typing.wlan import ClientStateSummary
from honeydew.typing.wlan import NetworkConfig
from honeydew.typing.wlan import SecurityType


class WlanPolicy(wlan_policy.WlanPolicy):
    """WlanPolicy affordance implementation using Fuchsia-Controller."""

    # List all the public methods in alphabetical order
    def create_client_controller(self) -> None:
        raise NotImplementedError

    def get_saved_networks(self) -> list[NetworkConfig]:
        raise NotImplementedError

    def get_update(self) -> ClientStateSummary:
        raise NotImplementedError

    def remove_all_networks(self) -> None:
        raise NotImplementedError

    def save_network(
            self,
            target_ssid: str,
            security_type: SecurityType,
            target_pwd: str | None = None) -> None:
        raise NotImplementedError

    def set_new_update_listener(self) -> None:
        raise NotImplementedError

    def start_client_connections(self) -> None:
        raise NotImplementedError

    def stop_client_connections(self) -> None:
        raise NotImplementedError
