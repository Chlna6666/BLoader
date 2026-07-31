[package]
name = "{{crate_name}}"
version = "0.1.0"
edition = "2024"
authors = ["{{author}}"]
description = "{{mod_name}} BL mod"
homepage = "https://example.invalid/{{crate_name}}"

[package.metadata.bl]
mod_id = "{{mod_id}}"
mod_name = "{{mod_name}}"
api_version = 1
entry = "mod.dll"
requires_symbol_pack = false
required_symbols = []

[lib]
crate-type = ["cdylib"]

[dependencies]
bl-sdk = { path = "{{sdk_path}}" }

[build-dependencies]
serde_json = "1.0"
toml = "0.8"
