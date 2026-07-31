pub mod abi;
pub mod host;
pub mod loader;

// Panel/ArcUI implementation is preserved for future work, but is excluded
// from the default DLL build so it cannot install input or rendering hooks.
#[cfg(feature = "panel-ui")]
pub mod adapter;
#[cfg(feature = "panel-ui")]
pub mod bedrock_ui;
#[cfg(feature = "panel-ui")]
pub mod client_state;
#[cfg(feature = "panel-ui")]
pub mod cursor_capture;
#[cfg(feature = "panel-ui")]
pub mod events;
#[cfg(feature = "panel-ui")]
pub mod loader_status;
#[cfg(feature = "panel-ui")]
pub mod text_overlay;
#[cfg(feature = "panel-ui")]
pub mod ui;
