use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() >= 2 && args[1] == "--from-cargo" {
        generate_from_cargo(&args);
        return;
    }

    if args.len() < 4 {
        eprintln!("Usage: cargo run --bin blgen -- <output_dir> <mod_id> <mod_name>");
        eprintln!("   or: cargo run --bin blgen -- --from-cargo <Cargo.toml> [output_dir]");
        std::process::exit(1);
    }

    let output_dir = PathBuf::from(&args[1]);
    let mod_id = &args[2];
    let mod_name = &args[3];
    let crate_name = sanitize_crate_name(mod_id);
    let dll_name = format!("{crate_name}.dll");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    fs::create_dir_all(output_dir.join("src")).expect("create output dir");
    let sdk_path = repo_root
        .join("bl-sdk")
        .to_string_lossy()
        .replace('\\', "/")
        .to_string();

    write_template(
        &repo_root
            .join("templates")
            .join("bl_mod")
            .join("Cargo.toml.tpl"),
        &output_dir.join("Cargo.toml"),
        mod_id,
        mod_name,
        &crate_name,
        &dll_name,
        &sdk_path,
    );
    write_template(
        &repo_root
            .join("templates")
            .join("bl_mod")
            .join("build.rs.tpl"),
        &output_dir.join("build.rs"),
        mod_id,
        mod_name,
        &crate_name,
        &dll_name,
        &sdk_path,
    );
    write_template(
        &repo_root
            .join("templates")
            .join("bl_mod")
            .join("manifest.json.tpl"),
        &output_dir.join("manifest.json"),
        mod_id,
        mod_name,
        &crate_name,
        &dll_name,
        &sdk_path,
    );
    write_template(
        &repo_root
            .join("templates")
            .join("bl_mod")
            .join("src")
            .join("lib.rs.tpl"),
        &output_dir.join("src").join("lib.rs"),
        mod_id,
        mod_name,
        &crate_name,
        &dll_name,
        &sdk_path,
    );

    println!("Generated BL mod template at {}", output_dir.display());
}

fn generate_from_cargo(args: &[String]) {
    if args.len() < 3 {
        eprintln!("Usage: cargo run --bin blgen -- --from-cargo <Cargo.toml> [output_dir]");
        std::process::exit(1);
    }

    let cargo_toml = PathBuf::from(&args[2]);
    let output_dir = args
        .get(3)
        .map(PathBuf::from)
        .or_else(|| cargo_toml.parent().map(|value| value.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let bundle = bl_sdk::project::generate_manifest_bundle(&cargo_toml).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    let output =
        bl_sdk::project::write_manifest_bundle(&bundle, &output_dir).unwrap_or_else(|error| {
            eprintln!("{error}");
            std::process::exit(2);
        });

    println!("Generated manifest bundle at {}", output.display());
}

fn write_template(
    template: &Path,
    destination: &Path,
    mod_id: &str,
    mod_name: &str,
    crate_name: &str,
    dll_name: &str,
    sdk_path: &str,
) {
    let text = fs::read_to_string(template).expect("read template");
    let rendered = text
        .replace("{{mod_id}}", mod_id)
        .replace("{{mod_name}}", mod_name)
        .replace("{{crate_name}}", crate_name)
        .replace("{{dll_name}}", dll_name)
        .replace("{{author}}", "BLoader")
        .replace("{{sdk_path}}", sdk_path);
    fs::write(destination, rendered).expect("write rendered template");
}

fn sanitize_crate_name(mod_id: &str) -> String {
    let mut out = String::with_capacity(mod_id.len());
    for ch in mod_id.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}
