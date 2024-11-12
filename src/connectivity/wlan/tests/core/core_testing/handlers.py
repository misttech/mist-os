# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Protocol handlers for antlion tests of WLAN core.
"""

import logging

logger = logging.getLogger(__name__)


import asyncio
from dataclasses import dataclass
from typing import Any

import fidl.fuchsia_wlan_sme as fidl_sme
from fuchsia_controller_py import Channel


@dataclass
class ConnectTransactionContext:
    txn_queue: asyncio.Queue[
        fidl_sme.ConnectTransactionOnConnectResultRequest
        | fidl_sme.ConnectTransactionOnDisconnectRequest
        | fidl_sme.ConnectTransactionOnRoamResultRequest
        | fidl_sme.ConnectTransactionOnSignalReportRequest
        | fidl_sme.ConnectTransactionOnChannelSwitchedRequest
    ]
    server: Channel


class ConnectTransactionEventHandler(fidl_sme.ConnectTransaction.EventHandler):
    def __init__(
        self,
        verbose: bool = True,
    ) -> None:
        self.verbose = verbose
        self.txn_queue: asyncio.Queue[
            fidl_sme.ConnectTransactionOnConnectResultRequest
            | fidl_sme.ConnectTransactionOnDisconnectRequest
            | fidl_sme.ConnectTransactionOnRoamResultRequest
            | fidl_sme.ConnectTransactionOnSignalReportRequest
            | fidl_sme.ConnectTransactionOnChannelSwitchedRequest
        ] = asyncio.Queue()

    def on_connect_result(
        self,
        request: fidl_sme.ConnectTransactionOnConnectResultRequest,
    ) -> None:
        if self.verbose:
            logger.info("Connect result: %s", request)
        self.txn_queue.put_nowait(request)

    def on_disconnect(
        self,
        request: fidl_sme.ConnectTransactionOnDisconnectRequest,
    ) -> None:
        if self.verbose:
            logger.info("Disconnect: %s", request)
        self.txn_queue.put_nowait(request)

    def on_roam_result(
        self,
        request: fidl_sme.ConnectTransactionOnRoamResultRequest,
    ) -> None:
        if self.verbose:
            logger.info("Roam result: %s", request)
        self.txn_queue.put_nowait(request)

    def on_signal_report(
        self,
        request: fidl_sme.ConnectTransactionOnSignalReportRequest,
    ) -> None:
        if self.verbose:
            logger.info("Signal report: %s", request)
        self.txn_queue.put_nowait(request)

    def on_channel_switched(
        self,
        request: fidl_sme.ConnectTransactionOnChannelSwitchedRequest,
    ) -> None:
        if self.verbose:
            logger.info("Channel switched: %s", request)
        self.txn_queue.put_nowait(request)

    def __enter__(self) -> ConnectTransactionContext:
        proxy, server = Channel.create()
        super().__init__(
            client=fidl_sme.ConnectTransaction.Client(proxy.take())
        )
        self.server_task = asyncio.get_running_loop().create_task(self.serve())
        return ConnectTransactionContext(
            txn_queue=self.txn_queue,
            server=server,
        )

    def __exit__(self, *args: Any, **kwargs: Any) -> None:
        if self.server_task:
            self.server_task.cancel()
