# Third-Party Notices

BLoader is distributed under GPL-3.0-or-later. The following components or interface descriptions informed portions of the implementation.

## WineGDK XUser and XAsync interfaces

The XUser interface identifiers, native structure layouts, 50-slot vtable ordering, XAsync provider operation ordering, and related GDK-compatible behavior in `src/core/xuser_bridge/` are Rust adaptations of material from WineGDK.

- Project: WineGDK
- Upstream: https://github.com/Weather-OS/WineGDK
- Relevant material: `include/xuser.idl`, `include/xasyncprovider.idl`, and `dlls/xgameruntime/`
- License: GNU Lesser General Public License 2.1 or later

BLoader's implementation is a new Rust implementation integrated into the loader. It does not include WineGDK's registry-based refresh-token transport or its incomplete token-signature stubs.

## Chlna6666/xgameruntime

The previous standalone Rust proxy was used as an implementation reference for GDK ABI validation, URL-to-relying-party routing, request-buffer validation, and Xbox proof-of-possession signing behavior.

- Project: xgameruntime
- Upstream: https://github.com/Chlna6666/xgameruntime
- License: GNU Lesser General Public License 2.1 or later

The BLoader bridge is implemented directly inside BLoader and does not link to or distribute the standalone proxy DLL.

## MinHook

BLoader uses the Rust `minhook` crate and the native MinHook library to intercept exactly one official function, `xgameruntime.dll!QueryApiImpl`, after an authenticated BMCBL session is accepted.

- Rust wrapper: https://github.com/Jakobzs/minhook — MIT License
- Native MinHook: https://github.com/TsudaKageyu/minhook — BSD 2-Clause License

Without an authenticated process-scoped session, no MinHook API is invoked and no XGameRuntime function is modified.
