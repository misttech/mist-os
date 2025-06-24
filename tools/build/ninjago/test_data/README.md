# Ninja Test Data

Test data is from a minimal.vim3 build.

Fuchsia at c365fdad3a1fec2877d95d13715ed423da73c55f

```bash
cd fuchsia
git checkout c365fdad3a1fec2877d95d13715ed423da73c55f
fx set core.x64
fx clean-build
cp out/default.zircon/.ninja_log ninja_log
ninja -C out/default.zircon -t compdb > compdb.json
ninja -C out/default.zircon -t graph gn > graph.dot
gzip ninja_log compdb.json graph.dot
```

For the `ninja_trace.json.gz` file:

```bash
cd fuchsia
git checkout 854ecb7a1f5c9b0c0cb939053086c463319c8498
fx set minimal.vim3
fx clean-build
rm -rf out/default/host_x64/ffx*unversioned
fx build -- --chrome_trace=ninja_trace.json.gz
cp -f out/default/ninja_trace.json.gz tools/build/ninjago/test_data/
```
