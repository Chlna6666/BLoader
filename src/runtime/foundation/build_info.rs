/// Visible BLoader build identity.
///
/// `NAME` and `PROFILE` are fixed product labels used by console banners, host
/// API and diagnostics. `VERSION` is sourced directly from `[package].version`
/// in Cargo.toml at compile time via `env!("CARGO_PKG_VERSION")`, so the two
/// values can never drift apart.
pub const NAME: &str = "BLoader";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PROFILE: &str = "native-preload-mod-crash-diagnostics";
