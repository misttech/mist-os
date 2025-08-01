Name: curl
URL: https://curl.haxx.se/
License: MIT/X derivate
License File: COPYING
Upstream Git: https://github.com/curl/curl
CPEPrefix: cpe:/a:haxx:curl:8.6.0
Description:

Curl is a command-line tool for transferring data specified with URL
syntax.

How to upgrade the version of curl:

  * `git tag` to look for the latest version.
  * `git merge curl-7_84_0`, resolve any conflict.
  * `autoreconf -fi`
  * `./configure --with-openssl --with-sysroot=$PWD/../../prebuilt/third_party/sysroot/linux`
  * `cp lib/curl_config.h lib/curl_config.h.host` and modify accordingly.
    * Mostly we want to only add or remove configs, not change any. Note that the same config file
      is used on both macOS and Linux. `cp lib/curl_config.h.host lib/curl_config.h.fuchsia` and
      modify accordingly.
  * Use `fx gen` to put these platform specific headers in the correct places in your out directory,
    and `fx build` to make sure there aren't any missing symbols.
  * Make sure you delete or rename the generated `lib/curl_config.h` when testing, or else your
    changes in `lib/curl_config.h.host` and `lib/curl_config.h.fuchsia` will not be picked up.

If you see missing symbols for `hugehelp`, you might need to regenerate `src/tool_hugehelp.c`:
  * Run the above configure steps..
  * `make -j10` will generate `src/tool_hugehelp.c`
  * Clean the intermediate curl object files from your out directory:
    `rm -rf $(fx get-build-dir)/{x64-shared,host_x64}/obj/third_party/curl/`
  * `fx gen` and `fx build` should resolve any missing symbols for `hugehelp`.
  * Commit the changes.
  * WARNING: `make clean` will delete `src/tool_hugehelp.c`.
