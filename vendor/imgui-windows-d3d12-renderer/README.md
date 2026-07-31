# imgui-windows-d3d12-renderer

D3D12 renderer for [imgui-rs](https://github.com/Gekkio/imgui-rs) implemented
using [windows-rs](https://github.com/microsoft/windows-rs).

[![ci](https://github.com/damyanp/imgui-windows-d3d12-renderer/actions/workflows/ci.yml/badge.svg)](https://github.com/damyanp/imgui-windows-d3d12-renderer/actions/workflows/ci.yml)
[![Latest release on
crates.io](https://img.shields.io/crates/v/imgui-windows-d3d12-renderer.svg)](https://crates.io/crates/imgui)

Looking for a rusty-d3d12 based renderer?  Check out [imgui-d3d12-renderer](https://github.com/curldivergence/imgui-d3d12-renderer).

## Usage

See [example](examples/hello_world.rs).

## Documentation

TBD

## License

Licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
  https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

at your option.

## Changelog

- 0.1.1
  - fix frame_count (that resulted in index buffers in use being destroyed)
  - add debug names for resources to aid debugging
- 0.1.0
  - initial release