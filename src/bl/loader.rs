use std::path::Path;

use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{HSTRING, PCSTR, PCWSTR};

use crate::bl::abi::{BL_API_VERSION_1, BlModMainFn};
use crate::bl::host;
use crate::runtime::foundation::{crash_report, logging, mod_diagnostics, native_stdio};

pub unsafe fn load_bl_mod(
    manifest_id: &str,
    name: &str,
    path: &Path,
    api_version: u32,
    version: Option<&str>,
    author: Option<&str>,
    description: Option<&str>,
    log_aliases: &[String],
) -> bool {
    let identity = mod_diagnostics::register_discovered(
        manifest_id.to_string(),
        name.to_string(),
        version.map(str::to_string),
        "BL".to_string(),
        path,
        log_aliases.to_vec(),
    );
    mod_diagnostics::mark_loading(&identity, "bl_load");
    let path_str = path.to_string_lossy().to_string();
    let wide = HSTRING::from(path_str.as_str());
    let load_result = {
        let _scope = mod_diagnostics::enter_scope(&identity, "LoadLibrary:bl_load");
        mod_diagnostics::record_lifecycle(&identity, "bl_entry_call", "LoadLibraryW");
        native_stdio::capture_library_load(&identity, "bl_load", || {
            LoadLibraryW(PCWSTR(wide.as_ptr()))
        })
    };
    crash_report::rearm_unhandled_filter(&format!("after-bl-load:{manifest_id}"));
    let module = match load_result {
        Ok(module) => module,
        Err(error) => {
            let detail = format!("{}", error.message());
            mod_diagnostics::mark_failed(&identity, "bl_load", &detail);
            logging::scoped_error_message(
                &format!("mod:{name}"),
                &format!(
                    "BL_LOAD_FAILURE | id={} path={} error={}",
                    manifest_id,
                    path.display(),
                    detail
                ),
            );
            return false;
        }
    };

    let entry = match GetProcAddress(module, PCSTR(c"bl_mod_main_v1".as_ptr() as *const u8)) {
        Some(ptr) => ptr,
        None => {
            let detail = "missing export bl_mod_main_v1";
            mod_diagnostics::mark_failed(&identity, "entry_verification", detail);
            logging::scoped_error_message(
                &format!("mod:{name}"),
                &format!("BL_LOAD_FAILURE | id={manifest_id} path={} {detail}", path.display()),
            );
            return false;
        }
    };

    let main_fn: BlModMainFn = std::mem::transmute(entry);
    let api = host::with_active_mod_identity(&identity, "bl_mod_main_v1", || {
        main_fn(host::host_api())
    });
    if api.is_null() {
        let detail = "bl_mod_main_v1 returned null API";
        mod_diagnostics::mark_failed(&identity, "bl_mod_main_v1", detail);
        logging::scoped_error_message(&format!("mod:{name}"), detail);
        return false;
    }

    let api = &*api;
    if api.api_version != BL_API_VERSION_1 {
        let detail = format!(
            "api version mismatch: expected {}, got {}",
            BL_API_VERSION_1, api.api_version
        );
        mod_diagnostics::mark_failed(&identity, "api_verification", &detail);
        logging::scoped_error_message(&format!("mod:{name}"), &detail);
        return false;
    }

    let mod_id = if api.mod_id.ptr.is_null() {
        manifest_id.to_string()
    } else {
        let bytes = std::slice::from_raw_parts(api.mod_id.ptr as *const u8, api.mod_id.len);
        String::from_utf8_lossy(bytes).to_string()
    };
    let mod_name = if api.mod_name.ptr.is_null() {
        name.to_string()
    } else {
        let bytes = std::slice::from_raw_parts(api.mod_name.ptr as *const u8, api.mod_name.len);
        String::from_utf8_lossy(bytes).to_string()
    };
    let runtime_identity = mod_diagnostics::register_discovered(
        mod_id.clone(),
        mod_name.clone(),
        version.map(str::to_string),
        "BL".to_string(),
        path,
        log_aliases.to_vec(),
    );

    if let Some(on_load) = api.on_load {
        let result = host::with_active_mod_identity(&runtime_identity, "on_load", || {
            on_load(host::host_api())
        });
        if result != 0 {
            logging::scoped_warn_message(
                &format!("mod:{mod_name}"),
                &format!("on_load returned {result}"),
            );
        }
    }

    host::register_loaded_mod(
        mod_id.clone(),
        mod_name.clone(),
        path_str,
        api_version,
        version.map(str::to_string),
        author.map(str::to_string),
        description.map(str::to_string),
        module.0 as usize,
        api.on_unload,
    );
    mod_diagnostics::mark_loaded(&runtime_identity, module.0 as usize, "bl_load");
    logging::scoped_info_message(
        &format!("mod:{mod_name}"),
        &format!(
            "BL_LOAD_SUCCESS | id={} version={} handle=0x{:X} path={}",
            mod_id,
            version.unwrap_or("unknown"),
            module.0 as usize,
            path.display()
        ),
    );
    true
}
