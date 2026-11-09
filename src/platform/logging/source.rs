//! Attribution for text captured from process-wide Win32 channels.
//!
//! `OutputDebugStringA/W` and the console device are shared by every module in
//! the process, so the owning module decides which source a captured line
//! belongs to. Only DLLs loaded from `mods/` are Mod sources; the game
//! executable, the DLLs shipped beside it and the Microsoft game runtime are
//! the game; the rest of Windows is System noise, not a Mod.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};

/// Microsoft game-runtime modules that live in System32 but belong to the
/// game's own stack (Xbox runtime and its HTTP transport), never to a Mod.
const GAME_RUNTIME_STEMS: &[&str] = &["xgameruntime", "libhttpclient", "xgamecore"];

/// Anything inside the Windows directory is an operating-system component:
/// System32/SysWOW64/WinSxS, but also host dirs like `Windows\UUS` (Windows
/// Update's wuapicore.dll) that a GDK game loads directly. A Mod never loads
/// from under `\Windows\`.
const SYSTEM_DIR_MARKERS: &[&str] = &["\\windows\\"];

/// CRT and Win32 shims that sit between the real caller and the hooked export.
/// Walking past them is what attributes `fwrite`-based Mod output to the Mod
/// instead of to ucrtbase.
const TRANSPARENT_STEMS: &[&str] = &[
    "kernel32",
    "kernelbase",
    "ntdll",
    "ucrtbase",
    "msvcrt",
    "vcruntime140",
    "vcruntime140_1",
    "msvcp140",
    "combase",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapturedSource {
    Game,
    Mod(String),
    System,
}

impl CapturedSource {
    pub fn scope(&self) -> String {
        match self {
            CapturedSource::Game => "game-log".to_string(),
            CapturedSource::Mod(name) => format!("mod:{name}"),
            CapturedSource::System => "system".to_string(),
        }
    }
}

/// Resolves the first stack frame that does not belong to a CRT/Win32 shim and
/// returns the scope it should be logged under (innermost frame first).
/// `None` means the call came from BLoader itself or from code that owns no
/// module (JIT/heap stubs) and must not be re-captured.
pub fn scope_for_frames(frames: &[usize]) -> Option<String> {
    let paths = frames
        .iter()
        .filter_map(|address| module_path_for_address(*address))
        .collect::<Vec<_>>();
    classify_frame_paths(&paths)
}

pub fn classify_frame_paths(paths: &[PathBuf]) -> Option<String> {
    classify_frame_paths_in(paths, exe_dir(), main_module())
}

fn classify_frame_paths_in(
    paths: &[PathBuf],
    exe_dir: &Path,
    main_module: &Path,
) -> Option<String> {
    for path in paths {
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if TRANSPARENT_STEMS.contains(&stem.as_str()) {
            continue;
        }
        return classify_path_in(path, exe_dir, main_module);
    }
    None
}

fn classify_path_in(path: &Path, exe_dir: &Path, main_module: &Path) -> Option<String> {
    let raw = path.to_string_lossy().to_string();
    if let Some(identity) = crate::platform::mod_diagnostics::find_by_module_path(&raw) {
        return Some(format!("mod:{}", identity.name));
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if let Some(identity) =
        crate::platform::mod_diagnostics::find_by_module_path(&canonical.to_string_lossy())
    {
        return Some(format!("mod:{}", identity.name));
    }
    classify_module_path(path, exe_dir, main_module).map(|source| source.scope())
}

fn exe_dir() -> &'static Path {
    static EXE_DIR: OnceLock<PathBuf> = OnceLock::new();
    EXE_DIR.get_or_init(crate::platform::utils::get_exe_directory)
}

fn main_module() -> &'static Path {
    static EXE_PATH: OnceLock<PathBuf> = OnceLock::new();
    EXE_PATH.get_or_init(crate::platform::utils::get_exe_path)
}

pub fn module_path_for_address(address: usize) -> Option<PathBuf> {
    if address == 0 {
        return None;
    }
    unsafe {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        if VirtualQuery(
            Some(address as *const _),
            &mut info,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        ) == 0
        {
            return None;
        }
        let module = HMODULE(info.AllocationBase);
        if module.is_invalid() {
            return None;
        }
        let path = crate::platform::utils::get_module_path(module.0 as usize);
        if path.as_os_str().is_empty() {
            return None;
        }
        Some(path)
    }
}

/// Pure classification so the ownership rules stay testable without a game
/// process. `exe_dir` is the directory of the running executable and
/// `main_module` its full path.
pub fn classify_module_path(
    path: &Path,
    exe_dir: &Path,
    main_module: &Path,
) -> Option<CapturedSource> {
    let stem = path.file_stem().and_then(|value| value.to_str())?.trim();
    if stem.is_empty() {
        return None;
    }
    let lower = stem.to_ascii_lowercase();
    if lower == "bloader"
        || lower.starts_with("bloader.")
        || lower.starts_with("bl-runtime")
        || lower.starts_with("bl_runtime")
    {
        // Never attribute BLoader's own output back to itself: the console
        // sink already wrote that line.
        return None;
    }

    let normalized_path = normalize(path);
    if normalized_path.eq_ignore_ascii_case(&normalize(main_module)) {
        return Some(CapturedSource::Game);
    }

    let normalized_exe_dir = normalize(exe_dir);
    if is_under(&normalized_path, &normalized_exe_dir) {
        // DLLs shipped beside the game executable are part of the game
        // install; everything under mods/ is a Mod, keyed by its file name
        // until the registry supplies a manifest name.
        if is_under(&normalized_path, &format!("{normalized_exe_dir}\\mods\\")) {
            return Some(CapturedSource::Mod(stem.to_string()));
        }
        return Some(CapturedSource::Game);
    }

    if GAME_RUNTIME_STEMS.contains(&lower.as_str()) {
        return Some(CapturedSource::Game);
    }

    if SYSTEM_DIR_MARKERS
        .iter()
        .any(|marker| normalized_path.to_ascii_lowercase().contains(marker))
    {
        return Some(CapturedSource::System);
    }

    // A module loaded from neither the game directory nor Windows is a
    // third-party component injected into the process; keep its own name.
    Some(CapturedSource::Mod(stem.to_string()))
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}

fn is_under(path: &str, directory: &str) -> bool {
    let directory = directory.trim_end_matches('\\');
    if directory.is_empty() {
        return false;
    }
    let prefix = format!("{directory}\\");
    path.len() > prefix.len() && path[..prefix.len()].eq_ignore_ascii_case(&prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe_dir() -> PathBuf {
        PathBuf::from(r"C:\Games\Minecraft")
    }

    fn main_module() -> PathBuf {
        PathBuf::from(r"C:\Games\Minecraft\Minecraft.Windows.exe")
    }

    fn classify(path: &str) -> Option<CapturedSource> {
        classify_module_path(Path::new(path), &exe_dir(), &main_module())
    }

    #[test]
    fn game_executable_and_shipped_dlls_are_game() {
        assert_eq!(
            classify(r"C:\Games\Minecraft\Minecraft.Windows.exe"),
            Some(CapturedSource::Game)
        );
        assert_eq!(
            classify(r"C:\Games\Minecraft\libHttpClient.dll"),
            Some(CapturedSource::Game)
        );
    }

    #[test]
    fn xgameruntime_is_the_game_not_a_mod() {
        assert_eq!(
            classify(r"C:\Windows\System32\xgameruntime.dll"),
            Some(CapturedSource::Game)
        );
    }

    #[test]
    fn mods_directory_is_a_mod() {
        assert_eq!(
            classify(r"C:\Games\Minecraft\mods\LeviLamina\LeviLamina.dll"),
            Some(CapturedSource::Mod("LeviLamina".to_string()))
        );
        assert_eq!(
            classify(r"C:\Games\Minecraft\mods\PreLoader\PreLoader.dll"),
            Some(CapturedSource::Mod("PreLoader".to_string()))
        );
    }

    #[test]
    fn windows_components_are_system_noise() {
        for path in [
            r"C:\Windows\System32\nlansp_c.dll",
            r"C:\Windows\System32\Windows.Media.Speech.dll",
            // Windows Update's core ships outside System32 (Windows\UUS).
            r"C:\Windows\UUS\amd64\wuapicore.dll",
        ] {
            assert_eq!(classify(path), Some(CapturedSource::System), "{path}");
        }
    }

    #[test]
    fn bloader_itself_is_never_captured() {
        assert_eq!(classify(r"C:\Games\Minecraft\BLoader.dll"), None);
    }

    #[test]
    fn crt_shims_are_walked_past_to_reach_the_mod() {
        let frames = vec![
            PathBuf::from(r"C:\Windows\System32\ucrtbase.dll"),
            PathBuf::from(r"C:\Windows\System32\kernelbase.dll"),
            PathBuf::from(r"C:\Games\Minecraft\mods\LeviLamina\LeviLamina.dll"),
        ];
        assert_eq!(
            classify_frame_paths_in(&frames, &exe_dir(), &main_module()),
            Some("mod:LeviLamina".to_string())
        );
    }

    #[test]
    fn crt_shims_are_walked_past_to_reach_the_game() {
        let frames = vec![
            PathBuf::from(r"C:\Windows\System32\ucrtbase.dll"),
            PathBuf::from(r"C:\Games\Minecraft\Minecraft.Windows.exe"),
        ];
        assert_eq!(
            classify_frame_paths_in(&frames, &exe_dir(), &main_module()),
            Some("game-log".to_string())
        );
    }

    #[test]
    fn scopes_render_as_console_sources() {
        assert_eq!(CapturedSource::Game.scope(), "game-log");
        assert_eq!(CapturedSource::System.scope(), "system");
        assert_eq!(
            CapturedSource::Mod("LeviLamina".to_string()).scope(),
            "mod:LeviLamina"
        );
    }
}
