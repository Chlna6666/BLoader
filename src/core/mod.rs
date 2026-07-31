pub mod console;
pub mod file_redirection;
pub mod loader;
pub mod network_hook;
#[cfg(feature = "panel-ui")]
pub mod global_input;

// Minimal graphics hook: only observes Present frames for delayed Mod loading
// and render-frame signals. It does not render ArcUI or capture input.
#[path = "render/render_signal.rs"]
pub mod render_signal;

#[path = "render/d3d12_queue.rs"]
pub mod d3d12_queue;

// Minecraft symbol loading is retained behind an opt-in feature and is not
// linked into the normal lightweight loader DLL.
#[cfg(feature = "mc-symbols")]
pub mod native_hud_discovery;
#[cfg(feature = "mc-symbols")]
pub mod sig_scan;
#[cfg(feature = "mc-symbols")]
pub mod symbol_diagnostics;
#[cfg(feature = "mc-symbols")]
pub mod symbols;

#[cfg(all(test, feature = "mc-symbols"))]
mod symbols_tests;
