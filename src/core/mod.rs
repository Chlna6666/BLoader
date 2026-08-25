pub mod console;
pub mod file_redirection;
pub mod launch_info;
pub mod loader;
pub mod network_hook;
pub mod pre_main_gate;
pub mod preloader_proxy;
pub mod runtime_ready;
#[path = "xuser_bridge/mod.rs"]
pub mod xuser_bridge;
#[cfg(feature = "panel-ui")]
pub mod global_input;

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
