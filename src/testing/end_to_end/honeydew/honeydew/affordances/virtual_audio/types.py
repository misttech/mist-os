# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""A base class for the audio recording affordance."""
import logging
from dataclasses import dataclass

import fidl_fuchsia_test_audio as fta
from fidl import AsyncSocket
from fuchsia_controller_py import Socket

from honeydew.affordances.virtual_audio.errors import VirtualAudioError

_LOGGER: logging.Logger = logging.getLogger(__name__)


@dataclass
class AudioInputWaiter:
    """AudioInputWaiter affordance response type.

    Args:
        endpoint: Fuchsia test audio InjectionClient endpoint.
    """

    def __init__(self, endpoint: fta.InjectionClient) -> None:
        self._injection_client: fta.InjectionClient = endpoint

    async def wait_until_injection_is_done(self) -> None:
        """Wait until the input has been played

        Raises:
            VirtualAudioError: On failure
        """
        _LOGGER.info("Waiting for audio injection to complete on the device...")
        if (
            err := (await self._injection_client.wait_until_input_is_done()).err
        ) is not None:
            raise VirtualAudioError(f"Failed to wait for audio {err}")
        _LOGGER.info("Audio injection has completed!")


@dataclass
class AudioResponse:
    """AudioResponse captured audio affordance response type.

    Args:
        endpoint: Fuchsia test audio CaptureClient endpoint.
    """

    def __init__(self, endpoint: fta.CaptureClient) -> None:
        """Init method for AudioResponse class"""
        self._capture_client: fta.CaptureClient = endpoint

    async def stop_and_extract_response(self) -> bytes:
        """Stops the audio capture of the output audio and extract the file.

        Return:
            bytes: Extracted audio response .wav ormat (Linear16 48Khz Channel 2)

        Raises:
            VirtualAudioError: On failure
        """

        _LOGGER.info("Stopping audio capture")
        if (
            err := (await self._capture_client.stop_output_capture()).err
        ) is not None:
            raise VirtualAudioError(f"Failed to stop output audio save {err}")
        _LOGGER.info("Audio capture stopped!")

        receiver = await self._capture_client.get_output_audio()
        if receiver.err is not None or receiver.response is None:
            raise VirtualAudioError(f"Failed to get audio: {receiver.err}")
        _LOGGER.info("Reading the stored audio data")
        sock = AsyncSocket(Socket(receiver.response.audio_reader))

        data = await sock.read_all()
        _LOGGER.info("Audio recording contains %d bytes", len(data))

        return bytes(data)
