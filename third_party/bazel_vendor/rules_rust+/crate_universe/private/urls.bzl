"""A file containing urls and associated sha256 values for cargo-bazel binaries

This file is auto-generated for each release to match the urls and sha256s of
the binaries produced for it.
"""

# Example:
# {
#     "x86_64-unknown-linux-gnu": "https://domain.com/downloads/cargo-bazel-x86_64-unknown-linux-gnu",
#     "x86_64-apple-darwin": "https://domain.com/downloads/cargo-bazel-x86_64-apple-darwin",
#     "x86_64-pc-windows-msvc": "https://domain.com/downloads/cargo-bazel-x86_64-pc-windows-msvc",
# }
CARGO_BAZEL_URLS = {
  "aarch64-apple-darwin": "https://github.com/bazelbuild/rules_rust/releases/download/0.59.1/cargo-bazel-aarch64-apple-darwin",
  "aarch64-pc-windows-msvc": "https://github.com/bazelbuild/rules_rust/releases/download/0.59.1/cargo-bazel-aarch64-pc-windows-msvc.exe",
  "aarch64-unknown-linux-gnu": "https://github.com/bazelbuild/rules_rust/releases/download/0.59.1/cargo-bazel-aarch64-unknown-linux-gnu",
  "x86_64-apple-darwin": "https://github.com/bazelbuild/rules_rust/releases/download/0.59.1/cargo-bazel-x86_64-apple-darwin",
  "x86_64-pc-windows-gnu": "https://github.com/bazelbuild/rules_rust/releases/download/0.59.1/cargo-bazel-x86_64-pc-windows-gnu.exe",
  "x86_64-pc-windows-msvc": "https://github.com/bazelbuild/rules_rust/releases/download/0.59.1/cargo-bazel-x86_64-pc-windows-msvc.exe",
  "x86_64-unknown-linux-gnu": "https://github.com/bazelbuild/rules_rust/releases/download/0.59.1/cargo-bazel-x86_64-unknown-linux-gnu",
  "x86_64-unknown-linux-musl": "https://github.com/bazelbuild/rules_rust/releases/download/0.59.1/cargo-bazel-x86_64-unknown-linux-musl"
}

# Example:
# {
#     "x86_64-unknown-linux-gnu": "1d687fcc860dc8a1aa6198e531f0aee0637ed506d6a412fe2b9884ff5b2b17c0",
#     "x86_64-apple-darwin": "0363e450125002f581d29cf632cc876225d738cfa433afa85ca557afb671eafa",
#     "x86_64-pc-windows-msvc": "f5647261d989f63dafb2c3cb8e131b225338a790386c06cf7112e43dd9805882",
# }
CARGO_BAZEL_SHA256S = {
  "aarch64-apple-darwin": "960c1a3c4d192c81e398bc7b33711dd456b2f444997028fb55b347a3ff543d92",
  "aarch64-pc-windows-msvc": "0d8470b42c6d24e9941481221f16adcafa6582dadf12d20ac9c74ba9368a9e7c",
  "aarch64-unknown-linux-gnu": "509f9eed09bc1b007eea8587cca5505fe61c0f24a714dff21e2d1a0224661d1a",
  "x86_64-apple-darwin": "c9639b80bddc715d0bf90173a0d840cc9141ed3775dca9341badd737a8b7ce38",
  "x86_64-pc-windows-gnu": "425efcf77106b5d104927e3c78515b86e373977c2360f47bad99247c746c3a87",
  "x86_64-pc-windows-msvc": "0d0945784ef5e7f2683412a07aaef65c46224de1d1ba3cc143bac09a24a5f018",
  "x86_64-unknown-linux-gnu": "4967f3f1ec7631e75193f1944e23c138499c171b568ea8e1f19a892db0c0b842",
  "x86_64-unknown-linux-musl": "2186f10f294b939de6a91b96e72c02f3de5d1dda965de79eea5b884d2f25723e"
}

# Example:
# Label("//crate_universe:cargo_bazel_bin")
CARGO_BAZEL_LABEL = Label("//crate_universe:cargo_bazel_bin")
