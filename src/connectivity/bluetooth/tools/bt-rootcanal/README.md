# bt-rootcanal

bt-rootcanal proxies HCI traffic between bt-host and an external virtual piconet provided by
[Rootcanal](https://source.android.com/docs/setup/create/cuttlefish-connectivity-bluetooth).

When executed, bt-rootcanal will attempt to connect to a host given a specified IP address and
optional port number (default 6402). After a successful connection, bt-rootcanal will spin up
a virtual Bluetooth controller made available to the system and proxy Bluetooth traffic between
the virtual controller and the Rootcanal server.

## How to use

To use this tool, include both the virtual controller and bt-rootcanal in the build:
```
$ fx set --with //src/connectivity/bluetooth/hci/virtual
    --args="base_driver_package_labels+=[\"//src/connectivity/bluetooth/hci/virtual\"]"
    --with //src/connectivity/bluetooth/tools/bt-rootcanal
```

Via fx shell, invoke bt-rootcanal with the IP address of the host running Rootcanal, e.g.
```
$ bt-rootcanal 172.16.243.1
```

In-line help can be accessed using the --help argument to the tool.
