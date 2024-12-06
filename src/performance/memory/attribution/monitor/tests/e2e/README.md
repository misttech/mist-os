This test exercises the various services offered by `memory_monitor2`:
- `ffx profile memory component`
- performance traces
- inspect data

Test it locally with:

```
fx set begonia_eng.x64 --release --with-host //src/performance/memory/attribution/monitor/tests/e2e:tests
fx build
fx ffx emu start -H
fx serve
```

In another terminal:

```
rm -rf /tmp/test/* /tmp/fctemp.* ; fx test --e2e --output --simple --ffx-output-directory /tmp/test //src/performance/memory/attribution/monitor/tests/e2e:memory_monitor2_e2e_test
```

Once it starts waiting for the device, open another terminal:

```
fx ffx --isolate-dir /tmp/fctemp.* emu start -H --console out/default/obj/vendor/google/products/begonia/begonia_eng.x64/product_bundle
```

Quit this one with `ctrl-a`, `ctrl-x`
All outputs can be found in `/tmp/test`.
