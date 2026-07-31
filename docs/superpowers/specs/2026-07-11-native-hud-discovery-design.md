# Native HUD Discovery for Minecraft Windows 26.21

## Goal

Render loader information through the Minecraft 26.21 native HUD text path,
instead of the DX12 ArcUI overlay.

The target display is:

- Bottom-left: `©Mojang AB / BMCBL` using the original copyright text style.
- Bottom-right: `BLoader <version> | <loaded-mod-count> mods`, placed eight
  pixels to the left of the original `v26.21` text.

## Scope

This change starts with a discovery-only runtime mode for the exact
`Minecraft.Windows.exe` 26.21 fingerprint and an optional generic discovery
profile for future `Minecraft.Windows.exe` versions. It may inspect and log
candidate functions, string cross-references, calling context, and observed text
layout. It must not write game memory, invoke an unverified game function, or
install a native HUD detour until its candidate passes the validation
requirements below.

The implementation may add:

- a focused native HUD discovery module;
- external `.blsym` metadata for verified native HUD symbols;
- diagnostic/runtime-info reporting for native HUD availability; and
- tests for parsing, validation decisions, and layout calculations that do not
  require the game process.

## Out of Scope

- Network packet hooks.
- ClientInstance or LocalPlayer function hooks.
- World, block, entity, and render-3D data access.
- Data-driven UI screen injection.
- Replacement of game resources or static strings.
- Enabling the native HUD renderer for any executable fingerprint other than a
  separately verified package.

## Runtime Model

The resolver obtains all game-specific values from an external `.blsym` package
next to `BLoader.dll`. The loader binary contains no addresses or patterns for
Minecraft UI functions. Exact packages match an executable SHA-256 and may
provide public symbols. A generic package matches only the executable name and
is marked discovery-only; it can never provide public symbols or enable hooks.

Discovery uses known HUD text anchors only to collect evidence. A candidate
becomes eligible for an enabled native hook only when all of these conditions
hold:

1. The pattern resolves exactly once in the target executable's `.text` section.
2. The candidate's bytes match the package validation bytes.
3. The candidate has the expected x64 ABI declaration and a known callback
   point for the text, position, color, scale, and screen context.
4. The discovered calls demonstrate both copyright and version text layout on
   the expected HUD frame path.
5. A non-writing trace mode has logged stable text coordinates and invocation
   counts across at least one game session.

If any condition fails, the hook remains disabled and diagnostics report
`native HUD unavailable`. The ArcUI overlay remains independent; it is not a
fallback for a partially installed native hook.

## Native Text Behaviour

When a verified hook is enabled, it delegates all non-target text to the game.
For the copyright call, it substitutes only the text value with
`©Mojang AB / BMCBL` and preserves the original drawing parameters. For the
version call, it preserves the original version draw and inserts the BLoader
label immediately before it using the same font metrics and baseline.

The mod count is read from BLoader's loaded-mod registry at draw time. The
native callback does not retain game object pointers across frames or worlds.

## External Symbol Package

The verified package will use a `native_hud.*` namespace. Each entry must state
its role, resolution method, section constraint, validation bytes, ABI revision,
and public exposure. Discovery-only entries are never exposed to MODs and never
install a hook. An enabled rendering entry is gated by the full validation set.

The initial 26.21 package remains limited to `runtime.game_module_base`; no
existing 26.3 candidate is eligible for promotion. A future generic discovery
package may hold patterns for evidence collection, but it does not replace a
verified per-fingerprint package.

## Diagnostics and Testing

The loader log and crash report will include native HUD mode, pack id, candidate
count, validation result, and a reason for rejection. The MOD runtime info API
will expose `ui.native_hud.status` only after the host has a real implementation.

Tests cover unique-candidate requirements, rejection of missing ABI evidence,
and bottom-left/bottom-right placement arithmetic. Manual verification requires
launching the target game with the exact package, observing the trace log, then
checking that the final native text uses the official HUD font and stays aligned
at multiple resolutions.
