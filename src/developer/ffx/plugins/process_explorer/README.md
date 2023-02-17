# ffx process_explorer

This `ffx` plugin allows retrieving information about Fuchsia processes.

In particular, the plugin can list channels, event and sockets held by processes
on a running Fuchsia device, allowing analysis of the topology of connections
between processes.

## Run

The plugin needs a support component not included in the build by default. Build
the Fuchsia image with the following additional package:

```
$ fx set [...] --with-base src/developer/process_explorer
```

Then run the plugin:

```
ffx process_explorer
```

## Usage example

```
$ ffx process_explorer list | grep archivist
  2257   archivist.cm

$ ffx process_explorer filter 2257
Total processes found:    1

Process name:             archivist.cm
Process koid:             2257
Total objects:            1139
   Processes: 1
         Koid:   2257    Related Koid:   2063    Peer Owner Koid:   2257
   Threads: 5
         Koid:   2848    Related Koid:   2257    Peer Owner Koid:      0
         Koid:   2762    Related Koid:   2257    Peer Owner Koid:      0
         Koid:   2751    Related Koid:   2257    Peer Owner Koid:      0
         Koid:   2735    Related Koid:   2257    Peer Owner Koid:      0
         Koid:   2262    Related Koid:   2257    Peer Owner Koid:      0
   VMOs: 47
         Koid:  53556    Related Koid:      0    Peer Owner Koid:      0
         Koid:  53552    Related Koid:      0    Peer Owner Koid:      0
         Koid:  53386    Related Koid:      0    Peer Owner Koid:      0
         Koid:  53287    Related Koid:      0    Peer Owner Koid:      0
         Koid:  29030    Related Koid:      0    Peer Owner Koid:      0
         Koid:   5495    Related Koid:      0    Peer Owner Koid:      0
         Koid:   5449    Related Koid:      0    Peer Owner Koid:      0
(...)
```