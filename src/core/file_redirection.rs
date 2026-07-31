use std::env;
use std::ffi::c_void;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use minhook::MinHook;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    CREATEFILE2_EXTENDED_PARAMETERS, FILE_CREATION_DISPOSITION, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_SHARE_MODE, FIND_FIRST_EX_FLAGS, FINDEX_INFO_LEVELS, FINDEX_SEARCH_OPS,
    GET_FILEEX_INFO_LEVELS, MOVE_FILE_FLAGS,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{BOOL, PCSTR, PCWSTR, s, w};

use crate::config::Config;
use crate::runtime::foundation::logging;

type CreateFileWFn = unsafe extern "system" fn(
    PCWSTR,
    u32,
    FILE_SHARE_MODE,
    *const SECURITY_ATTRIBUTES,
    FILE_CREATION_DISPOSITION,
    FILE_FLAGS_AND_ATTRIBUTES,
    HANDLE,
) -> HANDLE;
type CreateFileFromAppWFn = unsafe extern "system" fn(
    PCWSTR,
    u32,
    u32,
    *const SECURITY_ATTRIBUTES,
    u32,
    u32,
    HANDLE,
) -> HANDLE;
type CreateFile2Fn = unsafe extern "system" fn(
    PCWSTR,
    u32,
    FILE_SHARE_MODE,
    FILE_CREATION_DISPOSITION,
    *const CREATEFILE2_EXTENDED_PARAMETERS,
) -> HANDLE;
type CreateFile2FromAppWFn = unsafe extern "system" fn(
    PCWSTR,
    u32,
    u32,
    u32,
    *const CREATEFILE2_EXTENDED_PARAMETERS,
) -> HANDLE;
type CreateDirectoryWFn = unsafe extern "system" fn(PCWSTR, *const SECURITY_ATTRIBUTES) -> BOOL;
type DeleteFileWFn = unsafe extern "system" fn(PCWSTR) -> BOOL;
type RemoveDirectoryWFn = unsafe extern "system" fn(PCWSTR) -> BOOL;
type GetFileAttributesWFn = unsafe extern "system" fn(PCWSTR) -> u32;
type GetFileAttributesExWFn =
    unsafe extern "system" fn(PCWSTR, GET_FILEEX_INFO_LEVELS, *mut c_void) -> BOOL;
type FindFirstFileExWFn = unsafe extern "system" fn(
    PCWSTR,
    FINDEX_INFO_LEVELS,
    *mut c_void,
    FINDEX_SEARCH_OPS,
    *const c_void,
    FIND_FIRST_EX_FLAGS,
) -> HANDLE;
type FindFirstFileExFromAppWFn = unsafe extern "system" fn(
    PCWSTR,
    FINDEX_INFO_LEVELS,
    *mut c_void,
    FINDEX_SEARCH_OPS,
    *const c_void,
    u32,
) -> HANDLE;
type MoveFileExWFn = unsafe extern "system" fn(PCWSTR, PCWSTR, MOVE_FILE_FLAGS) -> BOOL;
type MoveFileFromAppWFn = unsafe extern "system" fn(PCWSTR, PCWSTR) -> BOOL;
type CopyFileWFn = unsafe extern "system" fn(PCWSTR, PCWSTR, BOOL) -> BOOL;
type SetFileAttributesWFn = unsafe extern "system" fn(PCWSTR, FILE_FLAGS_AND_ATTRIBUTES) -> BOOL;

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static REDIRECTION_STATE: OnceLock<RedirectionState> = OnceLock::new();

static ORIGINAL_CREATE_FILE_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_FILE_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_FILE2: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_FILE2_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_DIRECTORY_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_DIRECTORY_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_DELETE_FILE_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_DELETE_FILE_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_REMOVE_DIRECTORY_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_REMOVE_DIRECTORY_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_FILE_ATTRIBUTES_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_FILE_ATTRIBUTES_EX_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_FILE_ATTRIBUTES_EX_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_FIND_FIRST_FILE_EX_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_FIND_FIRST_FILE_EX_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_MOVE_FILE_EX_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_MOVE_FILE_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_COPY_FILE_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_COPY_FILE_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SET_FILE_ATTRIBUTES_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SET_FILE_ATTRIBUTES_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct RedirectionState {
    game_dir: PathBuf,
    rules: Vec<RuntimeRule>,
}

#[derive(Debug)]
struct RuntimeRule {
    source: String,
    source_key: String,
    source_child_key: String,
    target: String,
    directory: bool,
}

struct RedirectedWidePath {
    path: String,
    wide: Vec<u16>,
}

pub fn install(config: &Config, game_dir: &Path) -> bool {
    let state = RedirectionState::from_config(config, game_dir);
    if state.rules.is_empty() {
        if config.enable_redirection {
            logging::warn_message("[file-redirection] enabled but no usable rules were found.");
        } else {
            logging::info_message(
                "[file-redirection] no custom rules and data root redirection is disabled.",
            );
        }
        return false;
    }

    let rule_count = state.rules.len();
    let _ = REDIRECTION_STATE.set(state);

    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return true;
    }

    let installed = unsafe { install_file_hooks() };
    if installed == 0 {
        HOOK_INSTALLED.store(false, Ordering::SeqCst);
        logging::warn_message("[file-redirection] failed to install file API hooks.");
        return false;
    }

    if let Err(error) = unsafe { MinHook::enable_all_hooks() } {
        HOOK_INSTALLED.store(false, Ordering::SeqCst);
        logging::warn_message(&format!(
            "[file-redirection] failed to enable hooks: {error:?}"
        ));
        return false;
    }

    logging::info_message(&format!(
        "[file-redirection] installed {installed} hooks with {rule_count} rules."
    ));
    true
}

impl RedirectionState {
    fn from_config(config: &Config, game_dir: &Path) -> Self {
        let game_dir = game_dir.to_path_buf();
        let mut rules = Vec::new();

        for redirection in &config.file_redirections {
            let directory = redirection
                .kind
                .as_deref()
                .map(|kind| kind.eq_ignore_ascii_case("directory"))
                .unwrap_or(true);
            if let Some(rule) = RuntimeRule::from_config(
                &game_dir,
                &redirection.source,
                &redirection.target,
                directory,
            ) {
                rules.push(rule);
            }
        }

        if config.enable_redirection {
            let redirection_root = default_redirection_root(config, &game_dir);
            add_known_data_root_rules(&mut rules, &redirection_root);
        }

        Self { game_dir, rules }
    }

    fn redirect_path(&self, requested: &str) -> Option<String> {
        let requested_path = resolve_config_path(&self.game_dir, requested);
        let requested = normalize_match_path(&requested_path.to_string_lossy());
        let requested_key = requested.to_lowercase();

        for rule in &self.rules {
            if requested_key == rule.source_key {
                return Some(rule.target.clone());
            }

            if rule.directory && requested_key.starts_with(&rule.source_child_key) {
                let suffix = &requested[rule.source.len()..];
                return Some(format!("{}{}", rule.target, suffix));
            }
        }

        None
    }
}

impl RuntimeRule {
    fn from_config(game_dir: &Path, source: &str, target: &str, directory: bool) -> Option<Self> {
        if source.trim().is_empty() || target.trim().is_empty() {
            return None;
        }

        let source = normalize_match_path(&resolve_config_path(game_dir, source).to_string_lossy());
        let target = normalize_match_path(&resolve_config_path(game_dir, target).to_string_lossy());
        if source.is_empty() || target.is_empty() {
            return None;
        }

        let source_key = source.to_lowercase();
        let source_child_key = format!("{source_key}\\");

        Some(Self {
            source,
            source_key,
            source_child_key,
            target,
            directory,
        })
    }
}

fn default_redirection_root(config: &Config, game_dir: &Path) -> PathBuf {
    let configured = config.redirection_root.trim();
    if configured.is_empty() {
        game_dir.join("Minecraft Bedrock")
    } else {
        resolve_config_path(game_dir, configured)
    }
}

fn add_known_data_root_rules(rules: &mut Vec<RuntimeRule>, redirection_root: &Path) {
    if redirection_root.as_os_str().is_empty() {
        return;
    }

    if let Ok(appdata) = env::var("APPDATA") {
        let appdata = PathBuf::from(appdata);
        for source in [
            appdata.join("Minecraft Bedrock"),
            appdata.join("Minecraft Bedrock Preview"),
        ] {
            push_runtime_rule(rules, &source, redirection_root);
        }
    }

    if let Ok(local_appdata) = env::var("LOCALAPPDATA") {
        let packages = PathBuf::from(local_appdata).join("Packages");
        for package_name in [
            "Microsoft.MinecraftUWP_8wekyb3d8bbwe",
            "Microsoft.MinecraftWindowsBeta_8wekyb3d8bbwe",
            "Microsoft.MinecraftEducationEdition_8wekyb3d8bbwe",
            "Microsoft.MinecraftEducationEditionBeta_8wekyb3d8bbwe",
            "Microsoft.MinecraftEducationPreview_8wekyb3d8bbwe",
        ] {
            push_runtime_rule(
                rules,
                &packages.join(package_name).join("LocalState"),
                redirection_root,
            );
        }
    }
}

fn push_runtime_rule(rules: &mut Vec<RuntimeRule>, source: &Path, target: &Path) {
    if let Some(rule) = RuntimeRule::from_config(
        Path::new(""),
        &source.to_string_lossy(),
        &target.to_string_lossy(),
        true,
    ) {
        let duplicate = rules
            .iter()
            .any(|existing| existing.source_key == rule.source_key);
        if !duplicate {
            rules.push(rule);
        }
    }
}

fn resolve_config_path(game_dir: &Path, configured: &str) -> PathBuf {
    let path = strip_windows_namespace(configured);
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        game_dir.join(path)
    }
}

fn normalize_match_path(path: &str) -> String {
    let mut path = strip_windows_namespace(path);
    while path.len() > 3 && path.ends_with('\\') {
        path.pop();
    }
    path
}

fn strip_windows_namespace(path: &str) -> String {
    let path = path.trim().replace('/', "\\");
    let lower = path.to_ascii_lowercase();

    if lower.starts_with(r"\\?\unc\") {
        return format!(r"\\{}", &path[8..]);
    }
    if lower.starts_with(r"\??\unc\") {
        return format!(r"\\{}", &path[8..]);
    }
    if lower.starts_with(r"\\?\") || lower.starts_with(r"\??\") {
        return path[4..].to_string();
    }

    path
}

unsafe fn install_file_hooks() -> usize {
    let mut installed = 0;

    installed += hook_export(
        s!("CreateFileW"),
        "CreateFileW",
        detour_create_file_w as *mut c_void,
        &ORIGINAL_CREATE_FILE_W,
    ) as usize;
    installed += hook_export(
        s!("CreateFileFromAppW"),
        "CreateFileFromAppW",
        detour_create_file_from_app_w as *mut c_void,
        &ORIGINAL_CREATE_FILE_FROM_APP_W,
    ) as usize;
    installed += hook_export(
        s!("CreateFile2"),
        "CreateFile2",
        detour_create_file2 as *mut c_void,
        &ORIGINAL_CREATE_FILE2,
    ) as usize;
    installed += hook_export(
        s!("CreateFile2FromAppW"),
        "CreateFile2FromAppW",
        detour_create_file2_from_app_w as *mut c_void,
        &ORIGINAL_CREATE_FILE2_FROM_APP_W,
    ) as usize;
    installed += hook_export(
        s!("CreateDirectoryW"),
        "CreateDirectoryW",
        detour_create_directory_w as *mut c_void,
        &ORIGINAL_CREATE_DIRECTORY_W,
    ) as usize;
    installed += hook_export(
        s!("CreateDirectoryFromAppW"),
        "CreateDirectoryFromAppW",
        detour_create_directory_from_app_w as *mut c_void,
        &ORIGINAL_CREATE_DIRECTORY_FROM_APP_W,
    ) as usize;
    installed += hook_export(
        s!("DeleteFileW"),
        "DeleteFileW",
        detour_delete_file_w as *mut c_void,
        &ORIGINAL_DELETE_FILE_W,
    ) as usize;
    installed += hook_export(
        s!("DeleteFileFromAppW"),
        "DeleteFileFromAppW",
        detour_delete_file_from_app_w as *mut c_void,
        &ORIGINAL_DELETE_FILE_FROM_APP_W,
    ) as usize;
    installed += hook_export(
        s!("RemoveDirectoryW"),
        "RemoveDirectoryW",
        detour_remove_directory_w as *mut c_void,
        &ORIGINAL_REMOVE_DIRECTORY_W,
    ) as usize;
    installed += hook_export(
        s!("RemoveDirectoryFromAppW"),
        "RemoveDirectoryFromAppW",
        detour_remove_directory_from_app_w as *mut c_void,
        &ORIGINAL_REMOVE_DIRECTORY_FROM_APP_W,
    ) as usize;
    installed += hook_export(
        s!("GetFileAttributesW"),
        "GetFileAttributesW",
        detour_get_file_attributes_w as *mut c_void,
        &ORIGINAL_GET_FILE_ATTRIBUTES_W,
    ) as usize;
    installed += hook_export(
        s!("GetFileAttributesExW"),
        "GetFileAttributesExW",
        detour_get_file_attributes_ex_w as *mut c_void,
        &ORIGINAL_GET_FILE_ATTRIBUTES_EX_W,
    ) as usize;
    installed += hook_export(
        s!("GetFileAttributesExFromAppW"),
        "GetFileAttributesExFromAppW",
        detour_get_file_attributes_ex_from_app_w as *mut c_void,
        &ORIGINAL_GET_FILE_ATTRIBUTES_EX_FROM_APP_W,
    ) as usize;
    installed += hook_export(
        s!("FindFirstFileExW"),
        "FindFirstFileExW",
        detour_find_first_file_ex_w as *mut c_void,
        &ORIGINAL_FIND_FIRST_FILE_EX_W,
    ) as usize;
    installed += hook_export(
        s!("FindFirstFileExFromAppW"),
        "FindFirstFileExFromAppW",
        detour_find_first_file_ex_from_app_w as *mut c_void,
        &ORIGINAL_FIND_FIRST_FILE_EX_FROM_APP_W,
    ) as usize;
    installed += hook_export(
        s!("MoveFileExW"),
        "MoveFileExW",
        detour_move_file_ex_w as *mut c_void,
        &ORIGINAL_MOVE_FILE_EX_W,
    ) as usize;
    installed += hook_export(
        s!("MoveFileFromAppW"),
        "MoveFileFromAppW",
        detour_move_file_from_app_w as *mut c_void,
        &ORIGINAL_MOVE_FILE_FROM_APP_W,
    ) as usize;
    installed += hook_export(
        s!("CopyFileW"),
        "CopyFileW",
        detour_copy_file_w as *mut c_void,
        &ORIGINAL_COPY_FILE_W,
    ) as usize;
    installed += hook_export(
        s!("CopyFileFromAppW"),
        "CopyFileFromAppW",
        detour_copy_file_from_app_w as *mut c_void,
        &ORIGINAL_COPY_FILE_FROM_APP_W,
    ) as usize;
    installed += hook_export(
        s!("SetFileAttributesW"),
        "SetFileAttributesW",
        detour_set_file_attributes_w as *mut c_void,
        &ORIGINAL_SET_FILE_ATTRIBUTES_W,
    ) as usize;
    installed += hook_export(
        s!("SetFileAttributesFromAppW"),
        "SetFileAttributesFromAppW",
        detour_set_file_attributes_from_app_w as *mut c_void,
        &ORIGINAL_SET_FILE_ATTRIBUTES_FROM_APP_W,
    ) as usize;

    installed
}

unsafe fn hook_export(
    proc_name: PCSTR,
    label: &str,
    detour: *mut c_void,
    original: &AtomicUsize,
) -> bool {
    if original.load(Ordering::Acquire) != 0 {
        return false;
    }

    for module_name in [w!("KernelBase.dll"), w!("kernel32.dll")] {
        let Ok(module) = GetModuleHandleW(module_name) else {
            continue;
        };
        if module.is_invalid() {
            continue;
        }

        let Some(proc) = GetProcAddress(module, proc_name) else {
            continue;
        };

        match MinHook::create_hook(proc as *mut c_void, detour) {
            Ok(trampoline) => {
                original.store(trampoline as usize, Ordering::Release);
                return true;
            }
            Err(error) => {
                logging::warn_message(&format!(
                    "[file-redirection] failed to hook {label}: {error:?}"
                ));
                return false;
            }
        }
    }

    false
}

unsafe extern "system" fn detour_create_file_w(
    filename: PCWSTR,
    desired_access: u32,
    share_mode: FILE_SHARE_MODE,
    security_attributes: *const SECURITY_ATTRIBUTES,
    creation_disposition: FILE_CREATION_DISPOSITION,
    flags_and_attributes: FILE_FLAGS_AND_ATTRIBUTES,
    template_file: HANDLE,
) -> HANDLE {
    let redirected = redirect_pcwstr(filename);
    prepare_parent_for_redirect(redirected.as_ref());
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: CreateFileWFn = mem::transmute(ORIGINAL_CREATE_FILE_W.load(Ordering::Acquire));
    original(
        filename,
        desired_access,
        share_mode,
        security_attributes,
        creation_disposition,
        flags_and_attributes,
        template_file,
    )
}

unsafe extern "system" fn detour_create_file_from_app_w(
    filename: PCWSTR,
    desired_access: u32,
    share_mode: u32,
    security_attributes: *const SECURITY_ATTRIBUTES,
    creation_disposition: u32,
    flags_and_attributes: u32,
    template_file: HANDLE,
) -> HANDLE {
    let redirected = redirect_pcwstr(filename);
    prepare_parent_for_redirect(redirected.as_ref());
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: CreateFileFromAppWFn =
        mem::transmute(ORIGINAL_CREATE_FILE_FROM_APP_W.load(Ordering::Acquire));
    original(
        filename,
        desired_access,
        share_mode,
        security_attributes,
        creation_disposition,
        flags_and_attributes,
        template_file,
    )
}

unsafe extern "system" fn detour_create_file2(
    filename: PCWSTR,
    desired_access: u32,
    share_mode: FILE_SHARE_MODE,
    creation_disposition: FILE_CREATION_DISPOSITION,
    create_ex_params: *const CREATEFILE2_EXTENDED_PARAMETERS,
) -> HANDLE {
    let redirected = redirect_pcwstr(filename);
    prepare_parent_for_redirect(redirected.as_ref());
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: CreateFile2Fn = mem::transmute(ORIGINAL_CREATE_FILE2.load(Ordering::Acquire));
    original(
        filename,
        desired_access,
        share_mode,
        creation_disposition,
        create_ex_params,
    )
}

unsafe extern "system" fn detour_create_file2_from_app_w(
    filename: PCWSTR,
    desired_access: u32,
    share_mode: u32,
    creation_disposition: u32,
    create_ex_params: *const CREATEFILE2_EXTENDED_PARAMETERS,
) -> HANDLE {
    let redirected = redirect_pcwstr(filename);
    prepare_parent_for_redirect(redirected.as_ref());
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: CreateFile2FromAppWFn =
        mem::transmute(ORIGINAL_CREATE_FILE2_FROM_APP_W.load(Ordering::Acquire));
    original(
        filename,
        desired_access,
        share_mode,
        creation_disposition,
        create_ex_params,
    )
}

unsafe extern "system" fn detour_create_directory_w(
    pathname: PCWSTR,
    security_attributes: *const SECURITY_ATTRIBUTES,
) -> BOOL {
    let redirected = redirect_pcwstr(pathname);
    prepare_parent_for_redirect(redirected.as_ref());
    let pathname = redirected_pcwstr(pathname, redirected.as_ref());
    let original: CreateDirectoryWFn =
        mem::transmute(ORIGINAL_CREATE_DIRECTORY_W.load(Ordering::Acquire));
    original(pathname, security_attributes)
}

unsafe extern "system" fn detour_create_directory_from_app_w(
    pathname: PCWSTR,
    security_attributes: *const SECURITY_ATTRIBUTES,
) -> BOOL {
    let redirected = redirect_pcwstr(pathname);
    prepare_parent_for_redirect(redirected.as_ref());
    let pathname = redirected_pcwstr(pathname, redirected.as_ref());
    let original: CreateDirectoryWFn =
        mem::transmute(ORIGINAL_CREATE_DIRECTORY_FROM_APP_W.load(Ordering::Acquire));
    original(pathname, security_attributes)
}

unsafe extern "system" fn detour_delete_file_w(filename: PCWSTR) -> BOOL {
    let redirected = redirect_pcwstr(filename);
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: DeleteFileWFn = mem::transmute(ORIGINAL_DELETE_FILE_W.load(Ordering::Acquire));
    original(filename)
}

unsafe extern "system" fn detour_delete_file_from_app_w(filename: PCWSTR) -> BOOL {
    let redirected = redirect_pcwstr(filename);
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: DeleteFileWFn =
        mem::transmute(ORIGINAL_DELETE_FILE_FROM_APP_W.load(Ordering::Acquire));
    original(filename)
}

unsafe extern "system" fn detour_remove_directory_w(pathname: PCWSTR) -> BOOL {
    let redirected = redirect_pcwstr(pathname);
    let pathname = redirected_pcwstr(pathname, redirected.as_ref());
    let original: RemoveDirectoryWFn =
        mem::transmute(ORIGINAL_REMOVE_DIRECTORY_W.load(Ordering::Acquire));
    original(pathname)
}

unsafe extern "system" fn detour_remove_directory_from_app_w(pathname: PCWSTR) -> BOOL {
    let redirected = redirect_pcwstr(pathname);
    let pathname = redirected_pcwstr(pathname, redirected.as_ref());
    let original: RemoveDirectoryWFn =
        mem::transmute(ORIGINAL_REMOVE_DIRECTORY_FROM_APP_W.load(Ordering::Acquire));
    original(pathname)
}

unsafe extern "system" fn detour_get_file_attributes_w(filename: PCWSTR) -> u32 {
    let redirected = redirect_pcwstr(filename);
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: GetFileAttributesWFn =
        mem::transmute(ORIGINAL_GET_FILE_ATTRIBUTES_W.load(Ordering::Acquire));
    original(filename)
}

unsafe extern "system" fn detour_get_file_attributes_ex_w(
    filename: PCWSTR,
    info_level_id: GET_FILEEX_INFO_LEVELS,
    file_information: *mut c_void,
) -> BOOL {
    let redirected = redirect_pcwstr(filename);
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: GetFileAttributesExWFn =
        mem::transmute(ORIGINAL_GET_FILE_ATTRIBUTES_EX_W.load(Ordering::Acquire));
    original(filename, info_level_id, file_information)
}

unsafe extern "system" fn detour_get_file_attributes_ex_from_app_w(
    filename: PCWSTR,
    info_level_id: GET_FILEEX_INFO_LEVELS,
    file_information: *mut c_void,
) -> BOOL {
    let redirected = redirect_pcwstr(filename);
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: GetFileAttributesExWFn =
        mem::transmute(ORIGINAL_GET_FILE_ATTRIBUTES_EX_FROM_APP_W.load(Ordering::Acquire));
    original(filename, info_level_id, file_information)
}

unsafe extern "system" fn detour_find_first_file_ex_w(
    filename: PCWSTR,
    info_level_id: FINDEX_INFO_LEVELS,
    find_file_data: *mut c_void,
    search_op: FINDEX_SEARCH_OPS,
    search_filter: *const c_void,
    additional_flags: FIND_FIRST_EX_FLAGS,
) -> HANDLE {
    let redirected = redirect_pcwstr(filename);
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: FindFirstFileExWFn =
        mem::transmute(ORIGINAL_FIND_FIRST_FILE_EX_W.load(Ordering::Acquire));
    original(
        filename,
        info_level_id,
        find_file_data,
        search_op,
        search_filter,
        additional_flags,
    )
}

unsafe extern "system" fn detour_find_first_file_ex_from_app_w(
    filename: PCWSTR,
    info_level_id: FINDEX_INFO_LEVELS,
    find_file_data: *mut c_void,
    search_op: FINDEX_SEARCH_OPS,
    search_filter: *const c_void,
    additional_flags: u32,
) -> HANDLE {
    let redirected = redirect_pcwstr(filename);
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: FindFirstFileExFromAppWFn =
        mem::transmute(ORIGINAL_FIND_FIRST_FILE_EX_FROM_APP_W.load(Ordering::Acquire));
    original(
        filename,
        info_level_id,
        find_file_data,
        search_op,
        search_filter,
        additional_flags,
    )
}

unsafe extern "system" fn detour_move_file_ex_w(
    existing_filename: PCWSTR,
    new_filename: PCWSTR,
    flags: MOVE_FILE_FLAGS,
) -> BOOL {
    let redirected_existing = redirect_pcwstr(existing_filename);
    let redirected_new = redirect_pcwstr(new_filename);
    prepare_parent_for_redirect(redirected_new.as_ref());
    let existing_filename = redirected_pcwstr(existing_filename, redirected_existing.as_ref());
    let new_filename = redirected_pcwstr(new_filename, redirected_new.as_ref());
    let original: MoveFileExWFn = mem::transmute(ORIGINAL_MOVE_FILE_EX_W.load(Ordering::Acquire));
    original(existing_filename, new_filename, flags)
}

unsafe extern "system" fn detour_move_file_from_app_w(
    existing_filename: PCWSTR,
    new_filename: PCWSTR,
) -> BOOL {
    let redirected_existing = redirect_pcwstr(existing_filename);
    let redirected_new = redirect_pcwstr(new_filename);
    prepare_parent_for_redirect(redirected_new.as_ref());
    let existing_filename = redirected_pcwstr(existing_filename, redirected_existing.as_ref());
    let new_filename = redirected_pcwstr(new_filename, redirected_new.as_ref());
    let original: MoveFileFromAppWFn =
        mem::transmute(ORIGINAL_MOVE_FILE_FROM_APP_W.load(Ordering::Acquire));
    original(existing_filename, new_filename)
}

unsafe extern "system" fn detour_copy_file_w(
    existing_filename: PCWSTR,
    new_filename: PCWSTR,
    fail_if_exists: BOOL,
) -> BOOL {
    let redirected_existing = redirect_pcwstr(existing_filename);
    let redirected_new = redirect_pcwstr(new_filename);
    prepare_parent_for_redirect(redirected_new.as_ref());
    let existing_filename = redirected_pcwstr(existing_filename, redirected_existing.as_ref());
    let new_filename = redirected_pcwstr(new_filename, redirected_new.as_ref());
    let original: CopyFileWFn = mem::transmute(ORIGINAL_COPY_FILE_W.load(Ordering::Acquire));
    original(existing_filename, new_filename, fail_if_exists)
}

unsafe extern "system" fn detour_copy_file_from_app_w(
    existing_filename: PCWSTR,
    new_filename: PCWSTR,
    fail_if_exists: BOOL,
) -> BOOL {
    let redirected_existing = redirect_pcwstr(existing_filename);
    let redirected_new = redirect_pcwstr(new_filename);
    prepare_parent_for_redirect(redirected_new.as_ref());
    let existing_filename = redirected_pcwstr(existing_filename, redirected_existing.as_ref());
    let new_filename = redirected_pcwstr(new_filename, redirected_new.as_ref());
    let original: CopyFileWFn =
        mem::transmute(ORIGINAL_COPY_FILE_FROM_APP_W.load(Ordering::Acquire));
    original(existing_filename, new_filename, fail_if_exists)
}

unsafe extern "system" fn detour_set_file_attributes_w(
    filename: PCWSTR,
    file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
) -> BOOL {
    let redirected = redirect_pcwstr(filename);
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: SetFileAttributesWFn =
        mem::transmute(ORIGINAL_SET_FILE_ATTRIBUTES_W.load(Ordering::Acquire));
    original(filename, file_attributes)
}

unsafe extern "system" fn detour_set_file_attributes_from_app_w(
    filename: PCWSTR,
    file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
) -> BOOL {
    let redirected = redirect_pcwstr(filename);
    let filename = redirected_pcwstr(filename, redirected.as_ref());
    let original: SetFileAttributesWFn =
        mem::transmute(ORIGINAL_SET_FILE_ATTRIBUTES_FROM_APP_W.load(Ordering::Acquire));
    original(filename, file_attributes)
}

fn redirect_pcwstr(path: PCWSTR) -> Option<RedirectedWidePath> {
    if path.is_null() {
        return None;
    }

    let requested = unsafe { path.to_string().ok()? };
    let redirected = REDIRECTION_STATE.get()?.redirect_path(&requested)?;
    let wide = wide_null(&redirected);
    Some(RedirectedWidePath {
        path: redirected,
        wide,
    })
}

fn redirected_pcwstr(original: PCWSTR, redirected: Option<&RedirectedWidePath>) -> PCWSTR {
    redirected
        .map(|path| PCWSTR::from_raw(path.wide.as_ptr()))
        .unwrap_or(original)
}

fn prepare_parent_for_redirect(redirected: Option<&RedirectedWidePath>) {
    let Some(redirected) = redirected else {
        return;
    };
    if redirected.path.contains('*') || redirected.path.contains('?') {
        return;
    }
    if let Some(parent) = Path::new(&redirected.path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
}

fn wide_null(path: &str) -> Vec<u16> {
    std::ffi::OsStr::new(path)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_rule(source: &str, target: &str) -> RedirectionState {
        RedirectionState {
            game_dir: PathBuf::from(r"C:\Games\Minecraft"),
            rules: vec![
                RuntimeRule::from_config(Path::new(r"C:\Games\Minecraft"), source, target, true)
                    .unwrap(),
            ],
        }
    }

    #[test]
    fn redirects_exact_directory() {
        let state = state_with_rule(r"data\skin_packs\vanilla", r"C:\BMCBL\skin_packs\custom");
        assert_eq!(
            state.redirect_path(r"C:\Games\Minecraft\data\skin_packs\vanilla"),
            Some(r"C:\BMCBL\skin_packs\custom".to_string())
        );
    }

    #[test]
    fn redirects_child_path_and_preserves_suffix() {
        let state = state_with_rule(
            r"C:\Games\Minecraft\data\skin_packs\vanilla",
            r"C:\BMCBL\skin_packs\custom",
        );
        assert_eq!(
            state.redirect_path(r"\??\C:\Games\Minecraft\data\skin_packs\vanilla\skins.json"),
            Some(r"C:\BMCBL\skin_packs\custom\skins.json".to_string())
        );
    }

    #[test]
    fn does_not_redirect_sibling_prefix() {
        let state = state_with_rule(
            r"C:\Games\Minecraft\data\skin_packs\vanilla",
            r"C:\BMCBL\skin_packs\custom",
        );
        assert_eq!(
            state.redirect_path(r"C:\Games\Minecraft\data\skin_packs\vanilla_plus\skins.json"),
            None
        );
    }

    #[test]
    fn custom_rules_do_not_require_data_root_redirection() {
        let mut config = Config::default();
        config.enable_redirection = false;
        config
            .file_redirections
            .push(crate::config::FileRedirectionConfig {
                source: r"data\skin_packs\vanilla".to_string(),
                target: r"C:\BMCBL\skin_packs\custom".to_string(),
                kind: Some("directory".to_string()),
            });

        let state = RedirectionState::from_config(&config, Path::new(r"C:\Games\Minecraft"));

        assert_eq!(state.rules.len(), 1);
        assert_eq!(
            state.redirect_path(r"C:\Games\Minecraft\data\skin_packs\vanilla\skins.json"),
            Some(r"C:\BMCBL\skin_packs\custom\skins.json".to_string())
        );
    }
}
