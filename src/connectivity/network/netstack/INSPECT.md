# Netstack Inspect Data

Netstack exposes useful debugging information as inspectable data that can be
retrieved from snapshots (`inspect.json` within snapshots created by `fx
snapshot`) or by using the `iquery` tool (`fx iquery`).

In this document we list useful example queries into the inspect data that
netstack exposes.

We'll use the [`jq`] tool throughout, all queries can be used from an
`inspect.json` or `fx iquery --format json` by piping either into `jq`, e.g.:
```
cat snapshot/inspect.json | fx jq '...'
```
```
fx iquery --format json show core/network/netstack | fx jq '...'
```
## Inspect data

There are different sources of inspect data in Netstack, referenced by their
payload keys.

### Socket Info
`Socket Info` contains information of all currently open sockets, keyed by
socket identifier,  e.g.:
```json
{
  "12345": {
    "NetworkProtocol": "IPv6",
    "TransportProtocol": "UDP",
    "State": "BOUND",
    "LocalAddress": "[fe80::8d88:9942:4f32:7259]:546",
    "RemoteAddress": ":0",
    "BindAddress": "",
    "BindNICID": "5",
    "RegisterNICID": "5",
    "Stats": { ... },
    "Socket Option Stats": {
      "AddIpMembership": 0,
      ...
    },
  }
}
```

The `Socket Option Stats` section is keyed on each socket option operation
(either set or get, but note that some socket options only have one but not
the other), with the value indicating how many times the operation has been
performed. Note that there are socket options that only apply to certain
kinds of sockets, so the list of socket option operations differs between
the different socket types. The maximum across all sockets is also exposed,
see the [Networking Stat Counters](#networking-stat-counters) section.

To retrieve all sockets from inspect data use:
```
fx jq '.[] | select(.moniker == "core/network/netstack") | .payload."Socket Info" | .[]?'
```

### NICs
`NICs` contains information about each of the network interfaces presently
installed in the netstack, keyed by their interface identifier, e.g:
```json
{
  "2": {
    "Name": "wlanx62",
    "NICID": "3",
    "AdminUp": "true",
    "LinkOnline": "true",
    "Up": "true",
    "Running": "true",
    "Loopback": "false",
    "Promiscuous": "false",
    "LinkAddress": "44:07:0b:e2:cf:62",
    "ProtocolAddress0": "[arp] 617270/0",
    "ProtocolAddress1": "[ipv4] 192.168.0.23/24",
    "ProtocolAddress2": "[ipv6] fe80::916b:7615:6b2e:f8b3/64",
    "ProtocolAddress3": "[ipv6] fe80::4607:bff:fee2:cf62/128",
    "DHCP enabled": "true",
    "MTU": 1500,
    "Ethernet Info": { ... },
    "DHCP Info": { ... },
    "Stats": { ... }
  }
}
```

To retrieve all NICs from inspect data use:
```
fx jq '.[] | select(.moniker == "core/network/netstack") | .payload."NICs" | .[]?'
```
To look at a single NIC with `id` or `name` simply append `| select(.NICID ==
"id")` or `| select(.Name == "name")`, respectively.

### Networking Stat Counters {#networking-stat-counters}
`Networking Stat Counters` contain stack-global counters for traffic and errors,
e.g.:
```json
{
  "DroppedPackets": 0,
  "SocketCount": 12,
  "SocketsCreated": 1016568,
  "SocketsDestroyed": 1016559,
  "ARP": { ... },
  "DHCPv6": { ... },
  "ICMP": { ... },
  "IGMP": { ... },
  "IP": { ... },
  "IPv6AddressConfig": { ... },
  "MaxSocketOptionStats": { ... },
  "NICs": { ... },
  "TCP": {
    "ActiveConnectionOpenings": 4443,
    "PassiveConnectionOpenings": 53,
    ...
  },
  "UDP": {
    "PacketsReceived": 51551,
    "UnknownPortErrors": 30509,
    ...
  },
}
```

`MaxSocketOptionStats` is a map from each socket option operation to the
maximum number of times the operation has been called on a single socket in the
current boot. The max could come from a socket that's been closed, or is
currently open at the time inspect was queried.

To get counters use:
```
fx jq '.[] | select(.moniker == "core/network/netstack")
           | .payload."Networking Stat Counters"
           | select(. != null)'
```
You can append `| .TCP `, or `| .UDP` to look at only the subset of interest if
needed.

### Routes
`Routes` contains information about all the routes in the routing table, e.g.:
```json
{
  "0": {
    "Destination": "127.0.0.0/8",
    "Dynamic": "false",
    "Enabled": "true",
    "Gateway": "",
    "Metric": "100",
    "MetricTracksInterface": "true",
    "NIC": "1"
  },
}
```

To retrieve all routes from inspect data use:
```
fx jq '.[] | select(.moniker == "core/network/netstack") | .payload."Routes" | .[]?'
```

## pprof

Netstack exposes [`pprof`] data that can be used to gather more information from
the Go runtime.

A typical snapshot will contain periodic `pprof` data which is gathered at set
intervals. You can query the available `pprof` information with:
```
fx jq -c '.[] | select(.moniker == "core/network/netstack")
              | select(.payload.root.pprof != null)
              | {file: .metadata.filename, keys: (.payload.root.pprof | keys)}'
```
Which will generate output like
```
{"file":"pprof/2020-10-22T16:21:49Z.inspect","keys":["allocs","block","goroutine","heap","mutex","threadcreate"]}
{"file":"pprof/now.inspect","keys":["allocs","block","goroutine","heap","mutex","threadcreate"]}
{"file":"pprof/2020-10-22T16:19:49Z.inspect","keys":["allocs","block","goroutine","heap","mutex","threadcreate"]}
{"file":"pprof/2020-10-22T16:20:49Z.inspect","keys":["allocs","block","goroutine","heap","mutex","threadcreate"]}
```

Each of the `pprof` profiles is encoded in base64. The following line gets all
the `pprof` files and decodes them at once (remember to either pipe in your
`inspect` data or add path to your inspect file after the `jq` command):
```bash
fx jq -rc '.[]
            | select(.moniker == "core/network/netstack")
            | select(.payload.root.pprof != null)
            | . as $parent
            | (.payload.root.pprof | to_entries | .[] | ($parent | .metadata.filename) + "_" + .key + " " + .value)' | \
 while read f c; do echo $c | sed s/b64://g | base64 -d > $(echo $f | sed 's/pprof\///g'); done
```

You can modify the selector `.payload.root.pprof != null` and append `and
.metadata.filename == "pprof/now.inspect"` to only get the latest profile.

[`jq`]: https://stedolan.github.io/jq/
[`pprof`]: https://github.com/google/pprof

