# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Audio recording affordance."""
import asyncio
import logging
import os
import time

import fidl_fuchsia_test_audio as fta
from fuchsia_controller_py import Socket

from honeydew.affordances.virtual_audio import audio, errors, types
from honeydew.transports.fuchsia_controller import (
    fuchsia_controller as fc_transport,
)
from honeydew.typing.custom_types import FidlEndpoint

_LOGGER: logging.Logger = logging.getLogger(__name__)

_INJECTION_ENDPOINT = FidlEndpoint(
    "/core/audio_recording", "fuchsia.test.audio.Injection"
)

_CAPTURE_ENDPOINT = FidlEndpoint(
    "/core/audio_recording", "fuchsia.test.audio.Capture"
)


class VirtualAudioUsingFuchsiaController(audio.VirtualAudio):
    """Audio affordance implementation using Fuchsia Controller.

    Connecting to the protocols this connects to on startup will
    inject the virtual audio device which does the following things:

    - Input audio will only come from the virtual device. Actual microphones are disabled.
    - Output audio will only go to the virtual device. Actual speakers are disabled.

    TODO(https://fxbug.dev/417759272): There is currently no way to disable this
    behavior other than rebooting the device.

    Args:
        fc: Fuchsia Controller transport.
    """

    def __init__(
        self, fuchsia_controller: fc_transport.FuchsiaController
    ) -> None:
        self.fuchsia_controller: fc_transport.FuchsiaController = (
            fuchsia_controller
        )
        self._init_clients()

    def verify_supported(self) -> None:
        """Verify that virtual audio is supported for this device."""

        # TODO(https://fxbug.dev/418851327): Implement this function. For now just say it's supported.
        _LOGGER.info(
            "Saying virtual audio is supported, see https://fxbug.dev/418851327"
        )

    def _init_clients(self) -> None:
        self._injection_client = fta.InjectionClient(
            self.fuchsia_controller.connect_device_proxy(_INJECTION_ENDPOINT)
        )
        self._capture_client = fta.CaptureClient(
            self.fuchsia_controller.connect_device_proxy(_CAPTURE_ENDPOINT)
        )

    def inject(self, wav_file: str) -> types.AudioInputWaiter:
        """Inject wav_file audio query.
        Args:
            wav_file: Audio .wav file

        Return:
            AudioInputWaiter: object. This object is used to wait until the input injection is done.

        Raises:
            VirtualAudioError: On failure
            ValueError : Failed when input audio file is missing.
        """

        if not wav_file or not os.path.exists(wav_file):
            raise ValueError("Input audio file cannot be found.")

        audio_to_inject: bytes
        with open(wav_file, "rb") as f:
            audio_to_inject = f.read()

        async def _inject() -> None:
            val = await self._injection_client.clear_input_audio(index=0)
            if val.err is not None:
                raise errors.VirtualAudioError(
                    f"Failed to clear audio: {val.err}"
                )

            (sender, server_end) = Socket.create()
            self._injection_client.write_input_audio(
                index=0,
                audio_writer=server_end.take(),
            )

            start = time.monotonic()
            _LOGGER.info("Writing audio data...")
            sender.write(audio_to_inject)
            sender.close()
            _LOGGER.info(
                "...Done sending %d bytes in %fs",
                len(audio_to_inject),
                time.monotonic() - start,
            )

            size = await self._injection_client.get_input_audio_size(index=0)
            if size.err is not None or size.response is None:
                raise errors.VirtualAudioError(
                    f"Failed to get audio size: {size.err}"
                )

            if size.response.byte_count != len(audio_to_inject):
                raise errors.VirtualAudioError(
                    f"Expected to have written {size.response.byte_count} bytes, found {len(audio_to_inject)} at audio injection index 0"
                )

            _LOGGER.info("Saying it!")
            if (
                err := (
                    await self._injection_client.start_input_injection(index=0)
                ).err
            ) is not None:
                raise errors.VirtualAudioError(f"Failed to start audio {err}")

        asyncio.run(_inject())
        return types.AudioInputWaiter(self._injection_client)

    def capture(self) -> types.AudioResponse:
        """Start to capture the audio response.

        Return:
            AudioResponse: object. This object is used to stop and extract the captured audio.

        Raises:
            VirtualAudioError:  On failure
        """

        async def _capture() -> None:
            _LOGGER.info("Start audio capture...")
            if (
                err := (await self._capture_client.start_output_capture()).err
            ) is not None:
                raise errors.VirtualAudioError(
                    f"Failed to start output capture audio {err}"
                )

        asyncio.run(_capture())

        return types.AudioResponse(self._capture_client)
