use std::cell::Cell;
use std::collections::HashSet;
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
type CreateDirectoryExWFn =
    unsafe extern "system" fn(PCWSTR, PCWSTR, *const SECURITY_ATTRIBUTES) -> BOOL;
type DeleteFileWFn = unsafe extern "system" fn(PCWSTR) -> BOOL;
type RemoveDirectoryWFn = unsafe extern "system" fn(PCWSTR) -> BOOL;
type GetFileAttributesWFn = unsafe extern "system" fn(PCWSTR) -> u32;
type GetFileAttributesExWFn =
    unsafe extern "system" fn(PCWSTR, GET_FILEEX_INFO_LEVELS, *mut c_void) -> BOOL;
type FindFirstFileWFn = unsafe extern "system" fn(PCWSTR, *mut c_void) -> HANDLE;
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
type MoveFileWFn = unsafe extern "system" fn(PCWSTR, PCWSTR) -> BOOL;
type MoveFileExWFn = unsafe extern "system" fn(PCWSTR, PCWSTR, MOVE_FILE_FLAGS) -> BOOL;
type MoveFileFromAppWFn = unsafe extern "system" fn(PCWSTR, PCWSTR) -> BOOL;
type CopyFileWFn = unsafe extern "system" fn(PCWSTR, PCWSTR, BOOL) -> BOOL;
type ReplaceFileWFn = unsafe extern "system" fn(
    PCWSTR,
    PCWSTR,
    PCWSTR,
    u32,
    *mut c_void,
    *mut c_void,
) -> BOOL;
type SetFileAttributesWFn = unsafe extern "system" fn(PCWSTR, FILE_FLAGS_AND_ATTRIBUTES) -> BOOL;

type NtStatus = i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct NtUnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NtObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *mut NtUnicodeString,
    attributes: u32,
    security_descriptor: *mut c_void,
    security_quality_of_service: *mut c_void,
}

type NtCreateFileFn = unsafe extern "system" fn(
    *mut HANDLE,
    u32,
    *mut NtObjectAttributes,
    *mut c_void,
    *mut i64,
    u32,
    u32,
    u32,
    u32,
    *mut c_void,
    u32,
) -> NtStatus;
type NtOpenFileFn = unsafe extern "system" fn(
    *mut HANDLE,
    u32,
    *mut NtObjectAttributes,
    *mut c_void,
    u32,
    u32,
) -> NtStatus;
type NtDeleteFileFn = unsafe extern "system" fn(*mut NtObjectAttributes) -> NtStatus;
type NtQueryAttributesFileFn =
    unsafe extern "system" fn(*mut NtObjectAttributes, *mut c_void) -> NtStatus;
type NtSetInformationFileFn =
    unsafe extern "system" fn(HANDLE, *mut c_void, *mut c_void, u32, u32) -> NtStatus;

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static REDIRECTION_STATE: OnceLock<RedirectionState> = OnceLock::new();
static REDIRECT_LOG_COUNT: AtomicUsize = AtomicUsize::new(0);
const REDIRECT_LOG_LIMIT: usize = 4096;

static ORIGINAL_CREATE_FILE_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_FILE_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_FILE2: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_FILE2_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_DIRECTORY_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_DIRECTORY_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CREATE_DIRECTORY_EX_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_DELETE_FILE_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_DELETE_FILE_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_REMOVE_DIRECTORY_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_REMOVE_DIRECTORY_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_FILE_ATTRIBUTES_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_FILE_ATTRIBUTES_EX_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_GET_FILE_ATTRIBUTES_EX_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_FIND_FIRST_FILE_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_FIND_FIRST_FILE_EX_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_FIND_FIRST_FILE_EX_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_MOVE_FILE_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_MOVE_FILE_EX_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_MOVE_FILE_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_COPY_FILE_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_COPY_FILE_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_REPLACE_FILE_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SET_FILE_ATTRIBUTES_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SET_FILE_ATTRIBUTES_FROM_APP_W: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_NT_CREATE_FILE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_NT_OPEN_FILE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_NT_DELETE_FILE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_NT_QUERY_ATTRIBUTES_FILE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_NT_QUERY_FULL_ATTRIBUTES_FILE: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_NT_SET_INFORMATION_FILE: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static IN_FILE_REDIRECTION_HOOK: Cell<bool> = const { Cell::new(false) };
}

struct HookGuard;

impl HookGuard {
    fn enter() -> Option<Self> {
        IN_FILE_REDIRECTION_HOOK.with(|flag| {
            if flag.get() {
                None
            } else {
                flag.set(true);
                Some(Self)
            }
        })
    }
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        IN_FILE_REDIRECTION_HOOK.with(|flag| flag.set(false));
    }
}

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
    target_namespace: WindowsNamespace,
    directory: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsNamespace {
    None,
    Extended,
    Nt,
}

struct RedirectedWidePath {
    wide: Vec<u16>,
}

struct NtRedirectContext {
    wide: Vec<u16>,
    unicode: NtUnicodeString,
    attributes: NtObjectAttributes,
}

impl NtRedirectContext {
    fn object_attributes(&mut self) -> *mut NtObjectAttributes {
        self.unicode.buffer = self.wide.as_mut_ptr();
        self.attributes.object_name = &mut self.unicode;
        &mut self.attributes
    }
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

    prepare_rule_targets(&state);
    let rule_count = state.rules.len();
    for (index, rule) in state.rules.iter().enumerate() {
        logging::scoped_info_message(
            "file-redirection",
            &format!(
                "rule[{index}] priority=longest-first kind={} source={} target={} target_namespace={:?}",
                if rule.directory { "directory" } else { "file" },
                rule.source,
                rule.target,
                rule.target_namespace,
            ),
        );
    }

    if REDIRECTION_STATE.set(state).is_err() {
        logging::scoped_warn_message(
            "file-redirection",
            "redirection state was already initialized; keeping the first installed rule set",
        );
    }

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

    logging::scoped_info_message(
        "file-redirection",
        &format!(
            "installed {installed} hooks with {rule_count} rules | coverage=KernelBase+kernel32+ntdll | namespace=preserved | lexical_canonicalization=enabled | overlap=longest-first | hook_reentry=tls-bypass | hook_parent_creation=disabled | redirect_log_limit={REDIRECT_LOG_LIMIT}"
        ),
    );
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
            if let Some(redirection_root) = default_redirection_root(config, &game_dir) {
                add_known_data_root_rules(&mut rules, &redirection_root);
            }
        }

        rules.sort_by(|left, right| {
            right
                .source
                .len()
                .cmp(&left.source.len())
                .then_with(|| left.source_key.cmp(&right.source_key))
        });
        let mut seen = HashSet::new();
        rules.retain(|rule| seen.insert(rule.source_key.clone()));

        Self { game_dir, rules }
    }

    fn redirect_path(&self, requested: &str) -> Option<String> {
        self.redirect_path_impl(requested, true)
    }

    fn redirect_absolute_path(&self, requested: &str) -> Option<String> {
        self.redirect_path_impl(requested, false)
    }

    fn redirect_path_impl(&self, requested: &str, allow_relative: bool) -> Option<String> {
        let (input_namespace, stripped) = split_windows_namespace(requested);
        let requested = if is_absolute_windows_path(&stripped) {
            canonicalize_lexical_windows_path(&stripped)
        } else {
            if !allow_relative
                || input_namespace != WindowsNamespace::None
                || is_root_or_drive_relative(&stripped)
            {
                return None;
            }
            canonicalize_lexical_windows_path(
                &self.game_dir.join(stripped).to_string_lossy(),
            )
        };
        let requested_key = requested.to_ascii_lowercase();

        for rule in &self.rules {
            if requested_key == rule.source_key {
                let namespace = if input_namespace == WindowsNamespace::None {
                    rule.target_namespace
                } else {
                    input_namespace
                };
                return Some(apply_windows_namespace(namespace, &rule.target));
            }

            if rule.directory && requested_key.starts_with(&rule.source_child_key) {
                let suffix = requested.get(rule.source.len()..)?;
                let redirected = format!("{}{}", rule.target, suffix);
                let namespace = if input_namespace == WindowsNamespace::None {
                    rule.target_namespace
                } else {
                    input_namespace
                };
                return Some(apply_windows_namespace(namespace, &redirected));
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

        let (_, source) = resolve_configured_path(game_dir, source)?;
        let (target_namespace, target) = resolve_configured_path(game_dir, target)?;
        if source.is_empty() || target.is_empty() {
            return None;
        }

        let source_key = source.to_ascii_lowercase();
        let source_child_key = format!("{source_key}\\");

        Some(Self {
            source,
            source_key,
            source_child_key,
            target,
            target_namespace,
            directory,
        })
    }
}

fn default_redirection_root(config: &Config, game_dir: &Path) -> Option<String> {
    let configured = config.redirection_root.trim();
    if configured.is_empty() {
        Some(canonicalize_lexical_windows_path(
            &game_dir.join("Minecraft Bedrock").to_string_lossy(),
        ))
    } else {
        let (namespace, path) = resolve_configured_path(game_dir, configured)?;
        Some(apply_windows_namespace(namespace, &path))
    }
}

fn add_known_data_root_rules(rules: &mut Vec<RuntimeRule>, redirection_root: &str) {
    if redirection_root.is_empty() {
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

fn push_runtime_rule(rules: &mut Vec<RuntimeRule>, source: &Path, target: &str) {
    if let Some(rule) = RuntimeRule::from_config(
        Path::new(""),
        &source.to_string_lossy(),
        target,
        true,
    ) {
        rules.push(rule);
    }
}

fn prepare_rule_targets(state: &RedirectionState) {
    for rule in &state.rules {
        let target = Path::new(&rule.target);
        let directory = if rule.directory {
            Some(target)
        } else {
            target.parent()
        };
        let Some(directory) = directory else {
            continue;
        };
        if let Err(error) = std::fs::create_dir_all(directory) {
            logging::scoped_warn_message(
                "file-redirection",
                &format!(
                    "failed to prepare redirect target before hooks were enabled | target={} error={error}",
                    directory.display()
                ),
            );
        }
    }
}

fn resolve_configured_path(game_dir: &Path, configured: &str) -> Option<(WindowsNamespace, String)> {
    let (namespace, stripped) = split_windows_namespace(configured);
    if is_absolute_windows_path(&stripped) {
        return Some((namespace, canonicalize_lexical_windows_path(&stripped)));
    }
    if namespace != WindowsNamespace::None || is_root_or_drive_relative(&stripped) {
        return None;
    }
    Some((
        namespace,
        canonicalize_lexical_windows_path(&game_dir.join(stripped).to_string_lossy()),
    ))
}

fn split_windows_namespace(path: &str) -> (WindowsNamespace, String) {
    let path = path.trim().replace('/', "\\");
    let lower = path.to_ascii_lowercase();

    if lower.starts_with(r"\\?\unc\") {
        return (WindowsNamespace::Extended, format!(r"\\{}", &path[8..]));
    }
    if lower.starts_with(r"\??\unc\") {
        return (WindowsNamespace::Nt, format!(r"\\{}", &path[8..]));
    }
    if lower.starts_with(r"\\?\") {
        return (WindowsNamespace::Extended, path[4..].to_string());
    }
    if lower.starts_with(r"\??\") {
        return (WindowsNamespace::Nt, path[4..].to_string());
    }

    (WindowsNamespace::None, path)
}

fn apply_windows_namespace(namespace: WindowsNamespace, path: &str) -> String {
    match namespace {
        WindowsNamespace::None => path.to_string(),
        WindowsNamespace::Extended => {
            if path.starts_with(r"\\") {
                format!(r"\\?\UNC\{}", path.trim_start_matches('\\'))
            } else {
                format!(r"\\?\{path}")
            }
        }
        WindowsNamespace::Nt => {
            if path.starts_with(r"\\") {
                format!(r"\??\UNC\{}", path.trim_start_matches('\\'))
            } else {
                format!(r"\??\{path}")
            }
        }
    }
}

fn is_absolute_windows_path(path: &str) -> bool {
    if path.starts_with(r"\\") {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'\\'
}

fn is_root_or_drive_relative(path: &str) -> bool {
    if path.starts_with('\\') {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && (bytes.len() < 3 || bytes[2] != b'\\')
}

fn canonicalize_lexical_windows_path(path: &str) -> String {
    let path = path.trim().replace('/', "\\");
    if path.is_empty() {
        return path;
    }

    if let Some(rest) = path.strip_prefix(r"\\") {
        let mut components = rest.split('\\').filter(|component| !component.is_empty());
        let Some(server) = components.next() else {
            return r"\\".to_string();
        };
        let Some(share) = components.next() else {
            return format!(r"\\{server}");
        };
        let mut stack: Vec<&str> = Vec::new();
        for component in components {
            match component {
                "." => {}
                ".." => {
                    stack.pop();
                }
                _ => stack.push(component),
            }
        }
        let mut output = format!(r"\\{server}\{share}");
        for component in stack {
            output.push('\\');
            output.push_str(component);
        }
        return output;
    }

    let bytes = path.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'\\' {
        let drive = &path[..2];
        let mut stack: Vec<&str> = Vec::new();
        for component in path[3..].split('\\').filter(|component| !component.is_empty()) {
            match component {
                "." => {}
                ".." => {
                    stack.pop();
                }
                _ => stack.push(component),
            }
        }
        let mut output = format!(r"{drive}\");
        if !stack.is_empty() {
            output.push_str(&stack.join(r"\"));
        }
        return output;
    }

    let mut stack: Vec<&str> = Vec::new();
    for component in path.split('\\').filter(|component| !component.is_empty()) {
        match component {
            "." => {}
            ".." => {
                if stack.last().is_some_and(|last| *last != "..") {
                    stack.pop();
                } else {
                    stack.push(component);
                }
            }
            _ => stack.push(component),
        }
    }
    stack.join(r"\")
}

unsafe fn install_file_hooks() -> usize {
    let mut installed = 0;

    macro_rules! kernel_hook {
        ($name:literal, $detour:expr, $slot:expr) => {
            installed += hook_kernel_export(
                s!($name),
                $name,
                $detour as *mut c_void,
                $slot,
            ) as usize;
        };
    }
    macro_rules! ntdll_hook {
        ($name:literal, $detour:expr, $slot:expr) => {
            installed += hook_ntdll_export(
                s!($name),
                $name,
                $detour as *mut c_void,
                $slot,
            ) as usize;
        };
    }

    kernel_hook!("CreateFileW", detour_create_file_w, &ORIGINAL_CREATE_FILE_W);
    kernel_hook!(
        "CreateFileFromAppW",
        detour_create_file_from_app_w,
        &ORIGINAL_CREATE_FILE_FROM_APP_W
    );
    kernel_hook!("CreateFile2", detour_create_file2, &ORIGINAL_CREATE_FILE2);
    kernel_hook!(
        "CreateFile2FromAppW",
        detour_create_file2_from_app_w,
        &ORIGINAL_CREATE_FILE2_FROM_APP_W
    );
    kernel_hook!(
        "CreateDirectoryW",
        detour_create_directory_w,
        &ORIGINAL_CREATE_DIRECTORY_W
    );
    kernel_hook!(
        "CreateDirectoryFromAppW",
        detour_create_directory_from_app_w,
        &ORIGINAL_CREATE_DIRECTORY_FROM_APP_W
    );
    kernel_hook!(
        "CreateDirectoryExW",
        detour_create_directory_ex_w,
        &ORIGINAL_CREATE_DIRECTORY_EX_W
    );
    kernel_hook!("DeleteFileW", detour_delete_file_w, &ORIGINAL_DELETE_FILE_W);
    kernel_hook!(
        "DeleteFileFromAppW",
        detour_delete_file_from_app_w,
        &ORIGINAL_DELETE_FILE_FROM_APP_W
    );
    kernel_hook!(
        "RemoveDirectoryW",
        detour_remove_directory_w,
        &ORIGINAL_REMOVE_DIRECTORY_W
    );
    kernel_hook!(
        "RemoveDirectoryFromAppW",
        detour_remove_directory_from_app_w,
        &ORIGINAL_REMOVE_DIRECTORY_FROM_APP_W
    );
    kernel_hook!(
        "GetFileAttributesW",
        detour_get_file_attributes_w,
        &ORIGINAL_GET_FILE_ATTRIBUTES_W
    );
    kernel_hook!(
        "GetFileAttributesExW",
        detour_get_file_attributes_ex_w,
        &ORIGINAL_GET_FILE_ATTRIBUTES_EX_W
    );
    kernel_hook!(
        "GetFileAttributesExFromAppW",
        detour_get_file_attributes_ex_from_app_w,
        &ORIGINAL_GET_FILE_ATTRIBUTES_EX_FROM_APP_W
    );
    kernel_hook!(
        "FindFirstFileW",
        detour_find_first_file_w,
        &ORIGINAL_FIND_FIRST_FILE_W
    );
    kernel_hook!(
        "FindFirstFileExW",
        detour_find_first_file_ex_w,
        &ORIGINAL_FIND_FIRST_FILE_EX_W
    );
    kernel_hook!(
        "FindFirstFileExFromAppW",
        detour_find_first_file_ex_from_app_w,
        &ORIGINAL_FIND_FIRST_FILE_EX_FROM_APP_W
    );
    kernel_hook!("MoveFileW", detour_move_file_w, &ORIGINAL_MOVE_FILE_W);
    kernel_hook!("MoveFileExW", detour_move_file_ex_w, &ORIGINAL_MOVE_FILE_EX_W);
    kernel_hook!(
        "MoveFileFromAppW",
        detour_move_file_from_app_w,
        &ORIGINAL_MOVE_FILE_FROM_APP_W
    );
    kernel_hook!("CopyFileW", detour_copy_file_w, &ORIGINAL_COPY_FILE_W);
    kernel_hook!(
        "CopyFileFromAppW",
        detour_copy_file_from_app_w,
        &ORIGINAL_COPY_FILE_FROM_APP_W
    );
    kernel_hook!("ReplaceFileW", detour_replace_file_w, &ORIGINAL_REPLACE_FILE_W);
    kernel_hook!(
        "SetFileAttributesW",
        detour_set_file_attributes_w,
        &ORIGINAL_SET_FILE_ATTRIBUTES_W
    );
    kernel_hook!(
        "SetFileAttributesFromAppW",
        detour_set_file_attributes_from_app_w,
        &ORIGINAL_SET_FILE_ATTRIBUTES_FROM_APP_W
    );

    ntdll_hook!("NtCreateFile", detour_nt_create_file, &ORIGINAL_NT_CREATE_FILE);
    ntdll_hook!("NtOpenFile", detour_nt_open_file, &ORIGINAL_NT_OPEN_FILE);
    ntdll_hook!("NtDeleteFile", detour_nt_delete_file, &ORIGINAL_NT_DELETE_FILE);
    ntdll_hook!(
        "NtQueryAttributesFile",
        detour_nt_query_attributes_file,
        &ORIGINAL_NT_QUERY_ATTRIBUTES_FILE
    );
    ntdll_hook!(
        "NtQueryFullAttributesFile",
        detour_nt_query_full_attributes_file,
        &ORIGINAL_NT_QUERY_FULL_ATTRIBUTES_FILE
    );
    ntdll_hook!(
        "NtSetInformationFile",
        detour_nt_set_information_file,
        &ORIGINAL_NT_SET_INFORMATION_FILE
    );

    installed
}

unsafe fn hook_kernel_export(
    proc_name: PCSTR,
    label: &str,
    detour: *mut c_void,
    original: &AtomicUsize,
) -> bool {
    hook_export_from_modules(
        proc_name,
        label,
        detour,
        original,
        &[w!("KernelBase.dll"), w!("kernel32.dll")],
    )
}

unsafe fn hook_ntdll_export(
    proc_name: PCSTR,
    label: &str,
    detour: *mut c_void,
    original: &AtomicUsize,
) -> bool {
    hook_export_from_modules(proc_name, label, detour, original, &[w!("ntdll.dll")])
}

unsafe fn hook_export_from_modules(
    proc_name: PCSTR,
    label: &str,
    detour: *mut c_void,
    original: &AtomicUsize,
    module_names: &[PCWSTR],
) -> bool {
    if original.load(Ordering::Acquire) != 0 {
        return false;
    }

    for &module_name in module_names {
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
                logging::scoped_debug_message(
                    "file-redirection",
                    &format!("hook installed api={label} target=0x{:X}", proc as usize),
                );
                return true;
            }
            Err(error) => {
                logging::scoped_warn_message(
                    "file-redirection",
                    &format!("failed to hook {label}: {error:?}"),
                );
                return false;
            }
        }
    }

    logging::scoped_debug_message(
        "file-redirection",
        &format!("hook export unavailable api={label}"),
    );
    false
}

macro_rules! guard_or_call_original {
    ($original:expr $(, $arg:expr)* $(,)?) => {
        let Some(_redirect_guard) = HookGuard::enter() else {
            return $original($($arg),*);
        };
    };
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
    let original: CreateFileWFn = mem::transmute(ORIGINAL_CREATE_FILE_W.load(Ordering::Acquire));
    guard_or_call_original!(
        original,
        filename,
        desired_access,
        share_mode,
        security_attributes,
        creation_disposition,
        flags_and_attributes,
        template_file
    );
    let redirected = redirect_pcwstr("CreateFileW", filename);
    original(
        redirected_pcwstr(filename, redirected.as_ref()),
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
    let original: CreateFileFromAppWFn =
        mem::transmute(ORIGINAL_CREATE_FILE_FROM_APP_W.load(Ordering::Acquire));
    guard_or_call_original!(
        original,
        filename,
        desired_access,
        share_mode,
        security_attributes,
        creation_disposition,
        flags_and_attributes,
        template_file
    );
    let redirected = redirect_pcwstr("CreateFileFromAppW", filename);
    original(
        redirected_pcwstr(filename, redirected.as_ref()),
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
    let original: CreateFile2Fn = mem::transmute(ORIGINAL_CREATE_FILE2.load(Ordering::Acquire));
    guard_or_call_original!(
        original,
        filename,
        desired_access,
        share_mode,
        creation_disposition,
        create_ex_params
    );
    let redirected = redirect_pcwstr("CreateFile2", filename);
    original(
        redirected_pcwstr(filename, redirected.as_ref()),
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
    let original: CreateFile2FromAppWFn =
        mem::transmute(ORIGINAL_CREATE_FILE2_FROM_APP_W.load(Ordering::Acquire));
    guard_or_call_original!(
        original,
        filename,
        desired_access,
        share_mode,
        creation_disposition,
        create_ex_params
    );
    let redirected = redirect_pcwstr("CreateFile2FromAppW", filename);
    original(
        redirected_pcwstr(filename, redirected.as_ref()),
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
    let original: CreateDirectoryWFn =
        mem::transmute(ORIGINAL_CREATE_DIRECTORY_W.load(Ordering::Acquire));
    guard_or_call_original!(original, pathname, security_attributes);
    let redirected = redirect_pcwstr("CreateDirectoryW", pathname);
    original(
        redirected_pcwstr(pathname, redirected.as_ref()),
        security_attributes,
    )
}

unsafe extern "system" fn detour_create_directory_from_app_w(
    pathname: PCWSTR,
    security_attributes: *const SECURITY_ATTRIBUTES,
) -> BOOL {
    let original: CreateDirectoryWFn =
        mem::transmute(ORIGINAL_CREATE_DIRECTORY_FROM_APP_W.load(Ordering::Acquire));
    guard_or_call_original!(original, pathname, security_attributes);
    let redirected = redirect_pcwstr("CreateDirectoryFromAppW", pathname);
    original(
        redirected_pcwstr(pathname, redirected.as_ref()),
        security_attributes,
    )
}

unsafe extern "system" fn detour_create_directory_ex_w(
    template_directory: PCWSTR,
    new_directory: PCWSTR,
    security_attributes: *const SECURITY_ATTRIBUTES,
) -> BOOL {
    let original: CreateDirectoryExWFn =
        mem::transmute(ORIGINAL_CREATE_DIRECTORY_EX_W.load(Ordering::Acquire));
    guard_or_call_original!(
        original,
        template_directory,
        new_directory,
        security_attributes
    );
    let redirected_template = redirect_pcwstr("CreateDirectoryExW.template", template_directory);
    let redirected_new = redirect_pcwstr("CreateDirectoryExW.new", new_directory);
    original(
        redirected_pcwstr(template_directory, redirected_template.as_ref()),
        redirected_pcwstr(new_directory, redirected_new.as_ref()),
        security_attributes,
    )
}

unsafe extern "system" fn detour_delete_file_w(filename: PCWSTR) -> BOOL {
    let original: DeleteFileWFn = mem::transmute(ORIGINAL_DELETE_FILE_W.load(Ordering::Acquire));
    guard_or_call_original!(original, filename);
    let redirected = redirect_pcwstr("DeleteFileW", filename);
    original(redirected_pcwstr(filename, redirected.as_ref()))
}

unsafe extern "system" fn detour_delete_file_from_app_w(filename: PCWSTR) -> BOOL {
    let original: DeleteFileWFn =
        mem::transmute(ORIGINAL_DELETE_FILE_FROM_APP_W.load(Ordering::Acquire));
    guard_or_call_original!(original, filename);
    let redirected = redirect_pcwstr("DeleteFileFromAppW", filename);
    original(redirected_pcwstr(filename, redirected.as_ref()))
}

unsafe extern "system" fn detour_remove_directory_w(pathname: PCWSTR) -> BOOL {
    let original: RemoveDirectoryWFn =
        mem::transmute(ORIGINAL_REMOVE_DIRECTORY_W.load(Ordering::Acquire));
    guard_or_call_original!(original, pathname);
    let redirected = redirect_pcwstr("RemoveDirectoryW", pathname);
    original(redirected_pcwstr(pathname, redirected.as_ref()))
}

unsafe extern "system" fn detour_remove_directory_from_app_w(pathname: PCWSTR) -> BOOL {
    let original: RemoveDirectoryWFn =
        mem::transmute(ORIGINAL_REMOVE_DIRECTORY_FROM_APP_W.load(Ordering::Acquire));
    guard_or_call_original!(original, pathname);
    let redirected = redirect_pcwstr("RemoveDirectoryFromAppW", pathname);
    original(redirected_pcwstr(pathname, redirected.as_ref()))
}

unsafe extern "system" fn detour_get_file_attributes_w(filename: PCWSTR) -> u32 {
    let original: GetFileAttributesWFn =
        mem::transmute(ORIGINAL_GET_FILE_ATTRIBUTES_W.load(Ordering::Acquire));
    guard_or_call_original!(original, filename);
    let redirected = redirect_pcwstr("GetFileAttributesW", filename);
    original(redirected_pcwstr(filename, redirected.as_ref()))
}

unsafe extern "system" fn detour_get_file_attributes_ex_w(
    filename: PCWSTR,
    info_level_id: GET_FILEEX_INFO_LEVELS,
    file_information: *mut c_void,
) -> BOOL {
    let original: GetFileAttributesExWFn =
        mem::transmute(ORIGINAL_GET_FILE_ATTRIBUTES_EX_W.load(Ordering::Acquire));
    guard_or_call_original!(original, filename, info_level_id, file_information);
    let redirected = redirect_pcwstr("GetFileAttributesExW", filename);
    original(
        redirected_pcwstr(filename, redirected.as_ref()),
        info_level_id,
        file_information,
    )
}

unsafe extern "system" fn detour_get_file_attributes_ex_from_app_w(
    filename: PCWSTR,
    info_level_id: GET_FILEEX_INFO_LEVELS,
    file_information: *mut c_void,
) -> BOOL {
    let original: GetFileAttributesExWFn =
        mem::transmute(ORIGINAL_GET_FILE_ATTRIBUTES_EX_FROM_APP_W.load(Ordering::Acquire));
    guard_or_call_original!(original, filename, info_level_id, file_information);
    let redirected = redirect_pcwstr("GetFileAttributesExFromAppW", filename);
    original(
        redirected_pcwstr(filename, redirected.as_ref()),
        info_level_id,
        file_information,
    )
}

unsafe extern "system" fn detour_find_first_file_w(
    filename: PCWSTR,
    find_file_data: *mut c_void,
) -> HANDLE {
    let original: FindFirstFileWFn =
        mem::transmute(ORIGINAL_FIND_FIRST_FILE_W.load(Ordering::Acquire));
    guard_or_call_original!(original, filename, find_file_data);
    let redirected = redirect_pcwstr("FindFirstFileW", filename);
    original(
        redirected_pcwstr(filename, redirected.as_ref()),
        find_file_data,
    )
}

unsafe extern "system" fn detour_find_first_file_ex_w(
    filename: PCWSTR,
    info_level_id: FINDEX_INFO_LEVELS,
    find_file_data: *mut c_void,
    search_op: FINDEX_SEARCH_OPS,
    search_filter: *const c_void,
    additional_flags: FIND_FIRST_EX_FLAGS,
) -> HANDLE {
    let original: FindFirstFileExWFn =
        mem::transmute(ORIGINAL_FIND_FIRST_FILE_EX_W.load(Ordering::Acquire));
    guard_or_call_original!(
        original,
        filename,
        info_level_id,
        find_file_data,
        search_op,
        search_filter,
        additional_flags
    );
    let redirected = redirect_pcwstr("FindFirstFileExW", filename);
    original(
        redirected_pcwstr(filename, redirected.as_ref()),
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
    let original: FindFirstFileExFromAppWFn =
        mem::transmute(ORIGINAL_FIND_FIRST_FILE_EX_FROM_APP_W.load(Ordering::Acquire));
    guard_or_call_original!(
        original,
        filename,
        info_level_id,
        find_file_data,
        search_op,
        search_filter,
        additional_flags
    );
    let redirected = redirect_pcwstr("FindFirstFileExFromAppW", filename);
    original(
        redirected_pcwstr(filename, redirected.as_ref()),
        info_level_id,
        find_file_data,
        search_op,
        search_filter,
        additional_flags,
    )
}

unsafe extern "system" fn detour_move_file_w(
    existing_filename: PCWSTR,
    new_filename: PCWSTR,
) -> BOOL {
    let original: MoveFileWFn = mem::transmute(ORIGINAL_MOVE_FILE_W.load(Ordering::Acquire));
    guard_or_call_original!(original, existing_filename, new_filename);
    let redirected_existing = redirect_pcwstr("MoveFileW.source", existing_filename);
    let redirected_new = redirect_pcwstr("MoveFileW.target", new_filename);
    original(
        redirected_pcwstr(existing_filename, redirected_existing.as_ref()),
        redirected_pcwstr(new_filename, redirected_new.as_ref()),
    )
}

unsafe extern "system" fn detour_move_file_ex_w(
    existing_filename: PCWSTR,
    new_filename: PCWSTR,
    flags: MOVE_FILE_FLAGS,
) -> BOOL {
    let original: MoveFileExWFn = mem::transmute(ORIGINAL_MOVE_FILE_EX_W.load(Ordering::Acquire));
    guard_or_call_original!(original, existing_filename, new_filename, flags);
    let redirected_existing = redirect_pcwstr("MoveFileExW.source", existing_filename);
    let redirected_new = redirect_pcwstr("MoveFileExW.target", new_filename);
    original(
        redirected_pcwstr(existing_filename, redirected_existing.as_ref()),
        redirected_pcwstr(new_filename, redirected_new.as_ref()),
        flags,
    )
}

unsafe extern "system" fn detour_move_file_from_app_w(
    existing_filename: PCWSTR,
    new_filename: PCWSTR,
) -> BOOL {
    let original: MoveFileFromAppWFn =
        mem::transmute(ORIGINAL_MOVE_FILE_FROM_APP_W.load(Ordering::Acquire));
    guard_or_call_original!(original, existing_filename, new_filename);
    let redirected_existing = redirect_pcwstr("MoveFileFromAppW.source", existing_filename);
    let redirected_new = redirect_pcwstr("MoveFileFromAppW.target", new_filename);
    original(
        redirected_pcwstr(existing_filename, redirected_existing.as_ref()),
        redirected_pcwstr(new_filename, redirected_new.as_ref()),
    )
}

unsafe extern "system" fn detour_copy_file_w(
    existing_filename: PCWSTR,
    new_filename: PCWSTR,
    fail_if_exists: BOOL,
) -> BOOL {
    let original: CopyFileWFn = mem::transmute(ORIGINAL_COPY_FILE_W.load(Ordering::Acquire));
    guard_or_call_original!(original, existing_filename, new_filename, fail_if_exists);
    let redirected_existing = redirect_pcwstr("CopyFileW.source", existing_filename);
    let redirected_new = redirect_pcwstr("CopyFileW.target", new_filename);
    original(
        redirected_pcwstr(existing_filename, redirected_existing.as_ref()),
        redirected_pcwstr(new_filename, redirected_new.as_ref()),
        fail_if_exists,
    )
}

unsafe extern "system" fn detour_copy_file_from_app_w(
    existing_filename: PCWSTR,
    new_filename: PCWSTR,
    fail_if_exists: BOOL,
) -> BOOL {
    let original: CopyFileWFn =
        mem::transmute(ORIGINAL_COPY_FILE_FROM_APP_W.load(Ordering::Acquire));
    guard_or_call_original!(original, existing_filename, new_filename, fail_if_exists);
    let redirected_existing = redirect_pcwstr("CopyFileFromAppW.source", existing_filename);
    let redirected_new = redirect_pcwstr("CopyFileFromAppW.target", new_filename);
    original(
        redirected_pcwstr(existing_filename, redirected_existing.as_ref()),
        redirected_pcwstr(new_filename, redirected_new.as_ref()),
        fail_if_exists,
    )
}

unsafe extern "system" fn detour_replace_file_w(
    replaced_file: PCWSTR,
    replacement_file: PCWSTR,
    backup_file: PCWSTR,
    replace_flags: u32,
    exclude: *mut c_void,
    reserved: *mut c_void,
) -> BOOL {
    let original: ReplaceFileWFn = mem::transmute(ORIGINAL_REPLACE_FILE_W.load(Ordering::Acquire));
    guard_or_call_original!(
        original,
        replaced_file,
        replacement_file,
        backup_file,
        replace_flags,
        exclude,
        reserved
    );
    let redirected_replaced = redirect_pcwstr("ReplaceFileW.replaced", replaced_file);
    let redirected_replacement = redirect_pcwstr("ReplaceFileW.replacement", replacement_file);
    let redirected_backup = redirect_pcwstr("ReplaceFileW.backup", backup_file);
    original(
        redirected_pcwstr(replaced_file, redirected_replaced.as_ref()),
        redirected_pcwstr(replacement_file, redirected_replacement.as_ref()),
        redirected_pcwstr(backup_file, redirected_backup.as_ref()),
        replace_flags,
        exclude,
        reserved,
    )
}

unsafe extern "system" fn detour_set_file_attributes_w(
    filename: PCWSTR,
    file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
) -> BOOL {
    let original: SetFileAttributesWFn =
        mem::transmute(ORIGINAL_SET_FILE_ATTRIBUTES_W.load(Ordering::Acquire));
    guard_or_call_original!(original, filename, file_attributes);
    let redirected = redirect_pcwstr("SetFileAttributesW", filename);
    original(
        redirected_pcwstr(filename, redirected.as_ref()),
        file_attributes,
    )
}

unsafe extern "system" fn detour_set_file_attributes_from_app_w(
    filename: PCWSTR,
    file_attributes: FILE_FLAGS_AND_ATTRIBUTES,
) -> BOOL {
    let original: SetFileAttributesWFn =
        mem::transmute(ORIGINAL_SET_FILE_ATTRIBUTES_FROM_APP_W.load(Ordering::Acquire));
    guard_or_call_original!(original, filename, file_attributes);
    let redirected = redirect_pcwstr("SetFileAttributesFromAppW", filename);
    original(
        redirected_pcwstr(filename, redirected.as_ref()),
        file_attributes,
    )
}

unsafe extern "system" fn detour_nt_create_file(
    file_handle: *mut HANDLE,
    desired_access: u32,
    object_attributes: *mut NtObjectAttributes,
    io_status_block: *mut c_void,
    allocation_size: *mut i64,
    file_attributes: u32,
    share_access: u32,
    create_disposition: u32,
    create_options: u32,
    ea_buffer: *mut c_void,
    ea_length: u32,
) -> NtStatus {
    let original: NtCreateFileFn = mem::transmute(ORIGINAL_NT_CREATE_FILE.load(Ordering::Acquire));
    guard_or_call_original!(
        original,
        file_handle,
        desired_access,
        object_attributes,
        io_status_block,
        allocation_size,
        file_attributes,
        share_access,
        create_disposition,
        create_options,
        ea_buffer,
        ea_length
    );
    let mut redirected = redirect_nt_object_attributes("NtCreateFile", object_attributes);
    let attributes = redirected
        .as_mut()
        .map(NtRedirectContext::object_attributes)
        .unwrap_or(object_attributes);
    original(
        file_handle,
        desired_access,
        attributes,
        io_status_block,
        allocation_size,
        file_attributes,
        share_access,
        create_disposition,
        create_options,
        ea_buffer,
        ea_length,
    )
}

unsafe extern "system" fn detour_nt_open_file(
    file_handle: *mut HANDLE,
    desired_access: u32,
    object_attributes: *mut NtObjectAttributes,
    io_status_block: *mut c_void,
    share_access: u32,
    open_options: u32,
) -> NtStatus {
    let original: NtOpenFileFn = mem::transmute(ORIGINAL_NT_OPEN_FILE.load(Ordering::Acquire));
    guard_or_call_original!(
        original,
        file_handle,
        desired_access,
        object_attributes,
        io_status_block,
        share_access,
        open_options
    );
    let mut redirected = redirect_nt_object_attributes("NtOpenFile", object_attributes);
    let attributes = redirected
        .as_mut()
        .map(NtRedirectContext::object_attributes)
        .unwrap_or(object_attributes);
    original(
        file_handle,
        desired_access,
        attributes,
        io_status_block,
        share_access,
        open_options,
    )
}

unsafe extern "system" fn detour_nt_delete_file(
    object_attributes: *mut NtObjectAttributes,
) -> NtStatus {
    let original: NtDeleteFileFn = mem::transmute(ORIGINAL_NT_DELETE_FILE.load(Ordering::Acquire));
    guard_or_call_original!(original, object_attributes);
    let mut redirected = redirect_nt_object_attributes("NtDeleteFile", object_attributes);
    original(
        redirected
            .as_mut()
            .map(NtRedirectContext::object_attributes)
            .unwrap_or(object_attributes),
    )
}

unsafe extern "system" fn detour_nt_query_attributes_file(
    object_attributes: *mut NtObjectAttributes,
    file_information: *mut c_void,
) -> NtStatus {
    let original: NtQueryAttributesFileFn =
        mem::transmute(ORIGINAL_NT_QUERY_ATTRIBUTES_FILE.load(Ordering::Acquire));
    guard_or_call_original!(original, object_attributes, file_information);
    let mut redirected = redirect_nt_object_attributes("NtQueryAttributesFile", object_attributes);
    original(
        redirected
            .as_mut()
            .map(NtRedirectContext::object_attributes)
            .unwrap_or(object_attributes),
        file_information,
    )
}

unsafe extern "system" fn detour_nt_query_full_attributes_file(
    object_attributes: *mut NtObjectAttributes,
    file_information: *mut c_void,
) -> NtStatus {
    let original: NtQueryAttributesFileFn =
        mem::transmute(ORIGINAL_NT_QUERY_FULL_ATTRIBUTES_FILE.load(Ordering::Acquire));
    guard_or_call_original!(original, object_attributes, file_information);
    let mut redirected =
        redirect_nt_object_attributes("NtQueryFullAttributesFile", object_attributes);
    original(
        redirected
            .as_mut()
            .map(NtRedirectContext::object_attributes)
            .unwrap_or(object_attributes),
        file_information,
    )
}

unsafe extern "system" fn detour_nt_set_information_file(
    file_handle: HANDLE,
    io_status_block: *mut c_void,
    file_information: *mut c_void,
    length: u32,
    file_information_class: u32,
) -> NtStatus {
    let original: NtSetInformationFileFn =
        mem::transmute(ORIGINAL_NT_SET_INFORMATION_FILE.load(Ordering::Acquire));
    guard_or_call_original!(
        original,
        file_handle,
        io_status_block,
        file_information,
        length,
        file_information_class
    );
    let redirected = redirect_nt_rename_information(
        "NtSetInformationFile.rename",
        file_information,
        length,
        file_information_class,
    );
    if let Some(mut redirected) = redirected {
        return original(
            file_handle,
            io_status_block,
            redirected.as_mut_ptr().cast(),
            redirected.len() as u32,
            file_information_class,
        );
    }
    original(
        file_handle,
        io_status_block,
        file_information,
        length,
        file_information_class,
    )
}

fn redirect_pcwstr(api: &str, path: PCWSTR) -> Option<RedirectedWidePath> {
    if path.is_null() {
        return None;
    }

    let requested = unsafe { path.to_string().ok()? };
    let redirected = REDIRECTION_STATE.get()?.redirect_path(&requested)?;
    log_redirect(api, &requested, &redirected);
    Some(RedirectedWidePath {
        wide: wide_null(&redirected),
    })
}

fn redirected_pcwstr(original: PCWSTR, redirected: Option<&RedirectedWidePath>) -> PCWSTR {
    redirected
        .map(|path| PCWSTR::from_raw(path.wide.as_ptr()))
        .unwrap_or(original)
}

unsafe fn redirect_nt_object_attributes(
    api: &str,
    object_attributes: *mut NtObjectAttributes,
) -> Option<NtRedirectContext> {
    let attributes = object_attributes.as_ref()?;
    if !attributes.root_directory.0.is_null() || attributes.object_name.is_null() {
        return None;
    }
    let unicode = attributes.object_name.as_ref()?;
    if unicode.buffer.is_null() || unicode.length == 0 || unicode.length % 2 != 0 {
        return None;
    }
    let unit_count = usize::from(unicode.length) / 2;
    let requested = String::from_utf16(std::slice::from_raw_parts(unicode.buffer, unit_count)).ok()?;
    let redirected = REDIRECTION_STATE.get()?.redirect_absolute_path(&requested)?;
    let mut wide: Vec<u16> = redirected.encode_utf16().collect();
    let byte_len = wide.len().checked_mul(2)?;
    let length = u16::try_from(byte_len).ok()?;
    log_redirect(api, &requested, &redirected);

    let mut attributes_copy = *attributes;
    attributes_copy.object_name = std::ptr::null_mut();
    Some(NtRedirectContext {
        wide: mem::take(&mut wide),
        unicode: NtUnicodeString {
            length,
            maximum_length: length,
            buffer: std::ptr::null_mut(),
        },
        attributes: attributes_copy,
    })
}

unsafe fn redirect_nt_rename_information(
    api: &str,
    file_information: *mut c_void,
    length: u32,
    file_information_class: u32,
) -> Option<Vec<u8>> {
    const FILE_RENAME_INFORMATION_CLASS: u32 = 10;
    const FILE_RENAME_INFORMATION_EX_CLASS: u32 = 65;
    const RENAME_NAME_OFFSET_X64: usize = 20;

    if file_information_class != FILE_RENAME_INFORMATION_CLASS
        && file_information_class != FILE_RENAME_INFORMATION_EX_CLASS
    {
        return None;
    }
    if file_information.is_null() || (length as usize) < RENAME_NAME_OFFSET_X64 {
        return None;
    }

    let bytes = std::slice::from_raw_parts(file_information.cast::<u8>(), length as usize);
    let root_directory = std::ptr::read_unaligned(bytes.as_ptr().add(8).cast::<usize>());
    if root_directory != 0 {
        return None;
    }
    let name_length =
        std::ptr::read_unaligned(bytes.as_ptr().add(16).cast::<u32>()) as usize;
    if name_length == 0
        || name_length % 2 != 0
        || RENAME_NAME_OFFSET_X64.checked_add(name_length)? > bytes.len()
    {
        return None;
    }

    let name_bytes = &bytes[RENAME_NAME_OFFSET_X64..RENAME_NAME_OFFSET_X64 + name_length];
    let mut name_units = Vec::with_capacity(name_length / 2);
    for chunk in name_bytes.chunks_exact(2) {
        name_units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    let requested = String::from_utf16(&name_units).ok()?;
    let redirected = REDIRECTION_STATE.get()?.redirect_absolute_path(&requested)?;
    log_redirect(api, &requested, &redirected);

    let redirected_units: Vec<u16> = redirected.encode_utf16().collect();
    let redirected_name_length = redirected_units.len().checked_mul(2)?;
    let total_length = RENAME_NAME_OFFSET_X64.checked_add(redirected_name_length)?;
    let mut output = vec![0u8; total_length];
    output[..RENAME_NAME_OFFSET_X64]
        .copy_from_slice(&bytes[..RENAME_NAME_OFFSET_X64]);
    output[16..20].copy_from_slice(&(redirected_name_length as u32).to_le_bytes());
    for (index, unit) in redirected_units.iter().enumerate() {
        let offset = RENAME_NAME_OFFSET_X64 + index * 2;
        output[offset..offset + 2].copy_from_slice(&unit.to_le_bytes());
    }
    Some(output)
}

fn log_redirect(api: &str, source: &str, target: &str) {
    let index = REDIRECT_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    if index < REDIRECT_LOG_LIMIT {
        logging::scoped_debug_message(
            "file-redirection",
            &format!(
                "redirect seq={} api={} source={} target={}",
                index + 1,
                api,
                source,
                target
            ),
        );
    } else if index == REDIRECT_LOG_LIMIT {
        logging::scoped_warn_message(
            "file-redirection",
            &format!(
                "redirect detail log limit reached ({REDIRECT_LOG_LIMIT}); further redirect events are suppressed"
            ),
        );
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
        let mut rules = vec![
            RuntimeRule::from_config(Path::new(r"C:\Games\Minecraft"), source, target, true)
                .unwrap(),
        ];
        rules.sort_by(|left, right| right.source.len().cmp(&left.source.len()));
        RedirectionState {
            game_dir: PathBuf::from(r"C:\Games\Minecraft"),
            rules,
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
    fn redirects_child_path_and_preserves_nt_namespace() {
        let state = state_with_rule(
            r"C:\Games\Minecraft\data\skin_packs\vanilla",
            r"C:\BMCBL\skin_packs\custom",
        );
        assert_eq!(
            state.redirect_path(r"\??\C:\Games\Minecraft\data\skin_packs\vanilla\skins.json"),
            Some(r"\??\C:\BMCBL\skin_packs\custom\skins.json".to_string())
        );
    }

    #[test]
    fn redirects_child_path_and_preserves_extended_namespace() {
        let state = state_with_rule(
            r"C:\Games\Minecraft\data\skin_packs\vanilla",
            r"C:\BMCBL\skin_packs\custom",
        );
        assert_eq!(
            state.redirect_path(r"\\?\C:\Games\Minecraft\data\skin_packs\vanilla\skins.json"),
            Some(r"\\?\C:\BMCBL\skin_packs\custom\skins.json".to_string())
        );
    }

    #[test]
    fn canonicalization_resolves_dot_segments_before_matching() {
        let state = state_with_rule(
            r"C:\Games\Minecraft\data\skin_packs\vanilla",
            r"C:\BMCBL\skin_packs\custom",
        );
        assert_eq!(
            state.redirect_path(
                r"C:\Games\Minecraft\data\skin_packs\temp\..\vanilla\.\skins.json"
            ),
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
    fn longest_rule_wins_for_overlapping_sources() {
        let mut rules = vec![
            RuntimeRule::from_config(
                Path::new(r"C:\Games\Minecraft"),
                r"C:\Games\Minecraft\data",
                r"C:\Broad",
                true,
            )
            .unwrap(),
            RuntimeRule::from_config(
                Path::new(r"C:\Games\Minecraft"),
                r"C:\Games\Minecraft\data\skin_packs\vanilla",
                r"C:\Specific",
                true,
            )
            .unwrap(),
        ];
        rules.sort_by(|left, right| right.source.len().cmp(&left.source.len()));
        let state = RedirectionState {
            game_dir: PathBuf::from(r"C:\Games\Minecraft"),
            rules,
        };
        assert_eq!(
            state.redirect_path(r"C:\Games\Minecraft\data\skin_packs\vanilla\skins.json"),
            Some(r"C:\Specific\skins.json".to_string())
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