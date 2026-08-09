use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::System::Console::GetConsoleWindow;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetClientRect, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::BOOL;

use crate::runtime::foundation::logging;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const STABLE_WINDOW_DURATION: Duration = Duration::from_millis(750);
const MIN_CLIENT_WIDTH: i32 = 64;
const MIN_CLIENT_HEIGHT: i32 = 64;

static OEP_RELEASED: AtomicBool = AtomicBool::new(false);
static OEP_RELEASED_AT: OnceLock<Instant> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReadyLevel {
    Process,
    Window,
    StableWindow,
}

impl ReadyLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Window => "window",
            Self::StableWindow => "stable-window",
        }
    }

    pub fn from_manifest(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "process" => Some(Self::Process),
            "window" => Some(Self::Window),
            "stable-window" | "stable_window" | "stablewindow" => Some(Self::StableWindow),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReadySnapshot {
    pub hwnd: usize,
    pub width: i32,
    pub height: i32,
}

pub fn mark_oep_released(source: &str) {
    if !OEP_RELEASED.swap(true, Ordering::AcqRel) {
        let _ = OEP_RELEASED_AT.set(Instant::now());
        logging::write_bootstrap_marker(&format!(
            "runtime-ready.oep-released source={source} graphics_hooks=disabled"
        ));
    }
}

pub fn is_oep_released() -> bool {
    OEP_RELEASED.load(Ordering::Acquire)
}

pub fn wait_for(level: ReadyLevel, timeout: Duration) -> bool {
    let started = Instant::now();

    while !is_oep_released() {
        if started.elapsed() >= timeout {
            logging::scoped_warn_message(
                "runtime-ready",
                &format!(
                    "readiness timeout level={} stage=oep-release elapsed_ms={}",
                    level.as_str(),
                    started.elapsed().as_millis()
                ),
            );
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }

    if level == ReadyLevel::Process {
        logging::scoped_debug_message(
            "runtime-ready",
            &format!("ready level={} source=oep-release", level.as_str()),
        );
        return true;
    }

    let mut candidate_hwnd = 0usize;
    let mut candidate_since: Option<Instant> = None;

    loop {
        if let Some(snapshot) = find_game_window() {
            if level == ReadyLevel::Window {
                logging::scoped_info_message(
                    "runtime-ready",
                    &format!(
                        "ready level={} hwnd=0x{:X} client={}x{} elapsed_ms={} graphics_hooks=disabled",
                        level.as_str(),
                        snapshot.hwnd,
                        snapshot.width,
                        snapshot.height,
                        started.elapsed().as_millis()
                    ),
                );
                return true;
            }

            if snapshot.hwnd != candidate_hwnd {
                candidate_hwnd = snapshot.hwnd;
                candidate_since = Some(Instant::now());
                logging::scoped_debug_message(
                    "runtime-ready",
                    &format!(
                        "stable candidate hwnd=0x{:X} client={}x{} required_stable_ms={}",
                        snapshot.hwnd,
                        snapshot.width,
                        snapshot.height,
                        STABLE_WINDOW_DURATION.as_millis()
                    ),
                );
            } else if candidate_since
                .is_some_and(|since| since.elapsed() >= STABLE_WINDOW_DURATION)
            {
                logging::scoped_info_message(
                    "runtime-ready",
                    &format!(
                        "ready level={} hwnd=0x{:X} client={}x{} stable_ms={} elapsed_ms={} graphics_hooks=disabled",
                        level.as_str(),
                        snapshot.hwnd,
                        snapshot.width,
                        snapshot.height,
                        STABLE_WINDOW_DURATION.as_millis(),
                        started.elapsed().as_millis()
                    ),
                );
                return true;
            }
        } else {
            candidate_hwnd = 0;
            candidate_since = None;
        }

        if started.elapsed() >= timeout {
            logging::scoped_warn_message(
                "runtime-ready",
                &format!(
                    "readiness timeout level={} elapsed_ms={} graphics_hooks=disabled",
                    level.as_str(),
                    started.elapsed().as_millis()
                ),
            );
            return false;
        }

        thread::sleep(POLL_INTERVAL);
    }
}

pub fn wait_until_oep_delay(delay_ms: u64) {
    let Some(released_at) = OEP_RELEASED_AT.get().copied() else {
        return;
    };
    let target = Duration::from_millis(delay_ms);
    while released_at.elapsed() < target {
        let remaining = target.saturating_sub(released_at.elapsed());
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

pub fn find_game_window() -> Option<ReadySnapshot> {
    #[derive(Clone, Copy)]
    struct EnumData {
        pid: u32,
        console_hwnd: usize,
        best: Option<ReadySnapshot>,
        best_area: i64,
    }

    unsafe fn consider_window(hwnd: HWND, data: &mut EnumData) {
        let raw = hwnd.0 as usize;
        if raw == 0 || raw == data.console_hwnd || !IsWindowVisible(hwnd).as_bool() {
            return;
        }

        let mut window_pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut window_pid));
        if window_pid != data.pid {
            return;
        }

        let mut rect = RECT::default();
        if GetClientRect(hwnd, &mut rect).is_err() {
            return;
        }
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width < MIN_CLIENT_WIDTH || height < MIN_CLIENT_HEIGHT {
            return;
        }

        let area = i64::from(width) * i64::from(height);
        if area > data.best_area {
            data.best_area = area;
            data.best = Some(ReadySnapshot {
                hwnd: raw,
                width,
                height,
            });
        }
    }

    unsafe extern "system" fn enum_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let data = &mut *(lparam.0 as *mut EnumData);
        consider_window(hwnd, data);
        BOOL(1)
    }

    unsafe extern "system" fn enum_top(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let data = &mut *(lparam.0 as *mut EnumData);
        consider_window(hwnd, data);
        let _ = EnumChildWindows(Some(hwnd), Some(enum_child), lparam);
        BOOL(1)
    }

    let console_hwnd = unsafe { GetConsoleWindow() }.0 as usize;
    let mut data = EnumData {
        pid: unsafe { GetCurrentProcessId() },
        console_hwnd,
        best: None,
        best_area: 0,
    };

    unsafe {
        let _ = EnumWindows(
            Some(enum_top),
            LPARAM((&mut data as *mut EnumData) as isize),
        );
    }

    data.best
}

#[cfg(test)]
mod tests {
    use super::ReadyLevel;

    #[test]
    fn parses_manifest_ready_levels() {
        assert_eq!(ReadyLevel::from_manifest("process"), Some(ReadyLevel::Process));
        assert_eq!(ReadyLevel::from_manifest("window"), Some(ReadyLevel::Window));
        assert_eq!(
            ReadyLevel::from_manifest("stable-window"),
            Some(ReadyLevel::StableWindow)
        );
        assert_eq!(ReadyLevel::from_manifest("frames"), None);
    }
}
