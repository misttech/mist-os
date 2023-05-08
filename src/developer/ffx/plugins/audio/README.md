# ffx audio

## Usage

### Generating audio signals:
 sine, square, triangle,  sawtooth, pink noise and white noise. Output can be redirected to a file
 or piped directly into commands like ffx audio play or ffx audio device play.

`ffx audio gen [signal-type] --duration 5ms --frequency 440 --amplitude 0.5 --format 48000,int16,2ch`

### Using audio_core components:
AudioRenderer and AudioCapturer. Can specify usage, gain, mute, clock, and buffer size for
the shared VMO between the AudioDaemon on target and the AudioRenderer/AudioCapturer. Can use the
ultrasound renderer and capturer by specifying usage to ultrasound.

`ffx audio play`

`ffx audio record`

`ffx audio play --usage ultrasound`

`ffx audio record --usage ultrasound`

### Interacting with attached devices:
See what devices are connected. The device ID field is used for other device commands.

`ffx audio list-devices`
`ffx --machine json audio list-devices`

Print additional information about a specific device.

`ffx audio device --id {} --direction {input/output} info`

Play or record audio signal from hardware ring buffer. Note: core build required (audio_core
must not be present)

`ffx audio device --id {} record`

`ffx audio device --id {} play`

### Other Tips
* One can check the input/output of the signal by running a play and record command at the same
time from separate prompts and verify that the output of the record command matches what is
expected from the play command.

* Loop a file to pipe an ongoing audio signal to ffx audio play command:
ffmpeg -stream_loop <N> -i <filename> -c copy -f wav -

    Note that:
    <N> is the number of times to loop, or -1 to loop infinitely
    <filename> is the input wav file
    the file `-` outputs to stdout
    The following command continuously sends the file's wav contents to the device ring buffer.

`ffmpeg -stream_loop -1 -i <filename> -c copy -f wav - | ffx audio device play`

## Architecture

### ffxdaemon
Since interacting with the audio_core API’s and device ring buffers requires access to zircon
objects (VMO’s) and low latency writes to device ring buffers, we use a proxy component running
on target device to execute the audio commands on behalf of the ffx client. Daemon code can
be found at `/src/media/audio/services/ffxdaemon`.

The ffx host communicates with AudioDaemon through FIDL messages and sockets (passed via FIDL
messages), which are in `/sdk/fidl/fuchsia.audio.ffxdaemon/audio_proxy.fidl`. To add a new command,
one can add a high level subcommand on ffx audio or extend the existing subcommands like
play, record, etc. If the command needs to interact with the target device, one can add a new FIDL
method to `audio_proxy.fidl` or add fields to existing messages.

Audio data is transferred between host and target via `fidl::Socket` as bytestreams. Additionally,
any logging information from the ffxdaemon is sent back to the client via socket, which ffx then
copies it to the host device stderr.

Except for the gen subtool, all audio subtools interact with the ffxdaemon. In order to use these
subtools, add the following to `args.gn`.

`universe_package_labels += [ "//src/media/audio/services/ffxdaemon:audio_ffx_daemon" ]`
`core_realm_shards += [ "//src/media/audio/services/ffxdaemon:core_shard" ]`
