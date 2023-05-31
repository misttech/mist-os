# funnel

`funnel` takes a Fuchsia device in Product mode (i.e. not fastboot or
zedboot) and forwards the necessary ports from it over `ssh`. This allows for
you to develop in a remote workflow from your remote host.

## Usage

```
Usage: funnel -h <host> [-t <target-name>] [-r <repository-port>]

ffx Remote forwarding.

Options:
  -h, --host        the remote host to forward to
  -t, --target-name the name of the target to forward
  -r, --repository-port
                    the repository port to forward to the remote host
  --help            display usage information
```

## Notes

This binary is intended to replace the existing `fssh tunnel` command
([source](/tools/sdk-tools/fssh/tunnel/)).

## TODO

* Target selection when more than one target is detected.
* Additional port forwards
* Auto add and remove targets when the ssh connection is established/dropped.
