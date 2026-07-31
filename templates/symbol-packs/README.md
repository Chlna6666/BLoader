# External Symbol Packs

BLoader reads one shared compressed `.blsym` pack from the directory containing
`BLoader.dll`:

`<BLoader directory>/*.blsym`

An exact pack is selected when both `target.exe_name` and the lowercase SHA-256
of the running game executable match. Multiple exact matches are rejected.

`minecraft.windows.hud-discovery.example.json` is a generic discovery-only pack
for future `Minecraft.Windows.exe` builds. It omits `target.sha256`, cannot
declare ordinary symbols, cannot expose addresses to MODs, and cannot enable a
native hook. If both an exact pack and a generic discovery pack match, BLoader
always selects the exact pack.

`minecraft.windows.26.21.example.json` is a readable source pack matching the
supplied 26.21 executable. Compile it before deployment:

```powershell
python tools/pack_symbol_pack.py templates/symbol-packs/minecraft.windows.26.21.example.json <BLoader directory>/minecraft.windows.26.21.blsym
```

The generated 26.21 pack exposes only
`runtime.game_module_base`, which resolves to the loaded executable base after
checking for the PE `MZ` header.

Each symbol must use one of these resolution forms:

- `rva`: an RVA such as `"0x123456"`, checked to lie inside the loaded image.
- `pattern`: a space-separated byte pattern with `?` wildcards. It resolves only
  when exactly one match exists in the loaded image.

`validate` is an optional exact byte sequence checked at the resolved address.
Only entries with `"expose_to_mods": true` are returned by
`bl_sdk::mapping::resolve`; all other entries remain internal to the loader.

## Native HUD Discovery

`native_hud.candidates` is private loader metadata. A candidate must declare a
unique `.text` pattern, exact validation bytes, and an ABI description. Discovery
only records successful matches in the BLoader log; it does not call a candidate,
patch game memory, or install a hook.

Before a candidate can be promoted to a version-specific native HUD renderer,
capture a trace showing the copyright and version text calls with stable
coordinates, verify the x64 ABI, and manually review the result. Do not copy a
candidate from another game version into an exact pack.
