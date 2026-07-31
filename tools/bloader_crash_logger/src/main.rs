use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Local;
use windows::Win32::Foundation::{
    DBG_CONTINUE, DBG_EXCEPTION_NOT_HANDLED, EXCEPTION_BREAKPOINT, HANDLE, HMODULE,
};
use windows::Win32::System::Diagnostics::Debug::{
    AddrModeFlat, ADDRESS64, CONTEXT, CONTEXT_ALL_AMD64, ContinueDebugEvent, DEBUG_EVENT,
    DebugActiveProcess, DebugActiveProcessStop, EXCEPTION_DEBUG_EVENT, EXIT_PROCESS_DEBUG_EVENT,
    GetThreadContext, IMAGEHLP_LINEW64, STACKFRAME64, SYMBOL_INFO_PACKAGEW, SYMOPT_DEFERRED_LOADS,
    SYMOPT_LOAD_LINES, SYMOPT_UNDNAME, StackWalk64, SymFromAddrW, SymFunctionTableAccess64,
    SymGetLineFromAddrW64, SymGetModuleBase64, SymInitializeW, SymSetOptions, WaitForDebugEvent,
};
use windows::Win32::System::Memory::{VirtualQueryEx, MEMORY_BASIC_INFORMATION};
use windows::Win32::System::ProcessStatus::K32GetModuleFileNameExW;
use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_AMD64;
use windows::Win32::System::Threading::{
    OpenProcess, OpenThread, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ, THREAD_GET_CONTEXT,
    THREAD_QUERY_INFORMATION,
};
use windows::core::PCWSTR;

fn main() {
    let args = Args::parse();
    if let Err(error) = run(args) {
        let _ = write_side_log("startup-error", &error);
    }
}

struct Args {
    pid: u32,
    out_dir: PathBuf,
}

impl Args {
    fn parse() -> Self {
        let mut pid = 0u32;
        let mut out_dir = PathBuf::from("logs").join("crash-reports");
        let mut iter = env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--pid" => {
                    if let Some(value) = iter.next() {
                        pid = value.parse().unwrap_or(0);
                    }
                }
                "--out" => {
                    if let Some(value) = iter.next() {
                        out_dir = PathBuf::from(value);
                    }
                }
                _ => {}
            }
        }
        Self { pid, out_dir }
    }
}

fn run(args: Args) -> Result<(), String> {
    if args.pid == 0 {
        return Err("missing --pid".to_string());
    }
    fs::create_dir_all(&args.out_dir).map_err(|e| e.to_string())?;

    unsafe {
        DebugActiveProcess(args.pid).map_err(|e| format!("DebugActiveProcess failed: {e}"))?;
    }

    let result = debug_loop(args.pid, &args.out_dir);

    unsafe {
        let _ = DebugActiveProcessStop(args.pid);
    }

    result
}

fn debug_loop(pid: u32, out_dir: &Path) -> Result<(), String> {
    loop {
        let mut event = DEBUG_EVENT::default();
        unsafe {
            WaitForDebugEvent(&mut event, u32::MAX)
                .map_err(|e| format!("WaitForDebugEvent failed: {e}"))?;
        }

        let mut continue_status = DBG_CONTINUE;
        match event.dwDebugEventCode {
            EXCEPTION_DEBUG_EVENT => {
                let info = unsafe { event.u.Exception };
                let code = info.ExceptionRecord.ExceptionCode.0 as u32;
                let first_chance = info.dwFirstChance != 0;
                if code == EXCEPTION_BREAKPOINT.0 as u32 && first_chance {
                    continue_status = DBG_CONTINUE;
                } else if !first_chance {
                    let _ = capture_exception(pid, event.dwThreadId, &info.ExceptionRecord, out_dir);
                    continue_status = DBG_EXCEPTION_NOT_HANDLED;
                } else {
                    continue_status = DBG_EXCEPTION_NOT_HANDLED;
                }
            }
            EXIT_PROCESS_DEBUG_EVENT => {
                unsafe {
                    ContinueDebugEvent(event.dwProcessId, event.dwThreadId, continue_status)
                        .map_err(|e| format!("ContinueDebugEvent failed: {e}"))?;
                }
                break;
            }
            _ => {}
        }

        unsafe {
            ContinueDebugEvent(event.dwProcessId, event.dwThreadId, continue_status)
                .map_err(|e| format!("ContinueDebugEvent failed: {e}"))?;
        }
    }

    Ok(())
}

fn capture_exception(
    pid: u32,
    thread_id: u32,
    record: &windows::Win32::System::Diagnostics::Debug::EXCEPTION_RECORD,
    out_dir: &Path,
) -> Result<(), String> {
    let process = unsafe {
        OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid)
            .map_err(|e| format!("OpenProcess failed: {e}"))?
    };
    let thread = unsafe {
        OpenThread(THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION, false, thread_id)
            .map_err(|e| format!("OpenThread failed: {e}"))?
    };

    let mut context = CONTEXT::default();
    context.ContextFlags = CONTEXT_ALL_AMD64;
    unsafe {
        GetThreadContext(thread, &mut context).map_err(|e| format!("GetThreadContext failed: {e}"))?;
    }

    let exception_address = record.ExceptionAddress as usize;
    let timestamp = Local::now().format("%Y%m%d-%H%M%S-%3f").to_string();
    let report_path = out_dir.join(format!("crash-debugger-{timestamp}.txt"));

    let _ = ensure_symbols_initialized(process);
    let text = format!(
        "timestamp={}\r\nphase=debugger\r\nprocess_id={}\r\nthread_id={}\r\nexception_code=0x{:08X}\r\nexception_address=0x{:X}\r\nexception_module={}\r\nexception_symbol={}\r\nreport={}\r\n\r\n[registers]\r\n{}\r\n[stack]\r\n{}\r\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        pid,
        thread_id,
        record.ExceptionCode.0 as u32,
        exception_address,
        module_path_from_address(process, exception_address),
        symbolize_address(process, exception_address),
        report_path.display(),
        register_dump(&context),
        stack_trace(process, thread, &context),
    );

    write_report(&report_path, &text)?;
    Ok(())
}

fn ensure_symbols_initialized(process: HANDLE) -> bool {
    unsafe {
        let _ = SymSetOptions(SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES | SYMOPT_UNDNAME);
        SymInitializeW(process, PCWSTR::null(), true).is_ok()
    }
}

unsafe extern "system" fn stackwalk_function_table_access(
    process: HANDLE,
    address: u64,
) -> *mut core::ffi::c_void {
    unsafe { SymFunctionTableAccess64(process, address) }
}

unsafe extern "system" fn stackwalk_get_module_base(process: HANDLE, address: u64) -> u64 {
    unsafe { SymGetModuleBase64(process, address) }
}

fn stack_trace(process: HANDLE, thread: HANDLE, context: &CONTEXT) -> String {
    let mut ctx = *context;
    let mut frame = STACKFRAME64 {
        AddrPC: ADDRESS64 {
            Offset: ctx.Rip,
            Segment: 0,
            Mode: AddrModeFlat,
        },
        AddrReturn: ADDRESS64::default(),
        AddrFrame: ADDRESS64 {
            Offset: ctx.Rbp,
            Segment: 0,
            Mode: AddrModeFlat,
        },
        AddrStack: ADDRESS64 {
            Offset: ctx.Rsp,
            Segment: 0,
            Mode: AddrModeFlat,
        },
        AddrBStore: ADDRESS64::default(),
        FuncTableEntry: std::ptr::null_mut(),
        Params: [0; 4],
        Far: false.into(),
        Virtual: false.into(),
        Reserved: [0; 3],
        KdHelp: windows::Win32::System::Diagnostics::Debug::KDHELP64::default(),
    };

    let mut lines = Vec::new();
    for index in 0..64 {
        let result = unsafe {
            StackWalk64(
                IMAGE_FILE_MACHINE_AMD64.0 as u32,
                process,
                thread,
                &mut frame,
                &mut ctx as *mut _ as *mut _,
                None,
                Some(stackwalk_function_table_access),
                Some(stackwalk_get_module_base),
                None,
            )
        };
        if !result.as_bool() || frame.AddrPC.Offset == 0 {
            break;
        }
        lines.push(format!("#{} {}", index, symbolize_address(process, frame.AddrPC.Offset as usize)));
    }

    if lines.is_empty() {
        "<empty>".to_string()
    } else {
        lines.join("\r\n")
    }
}

fn symbolize_address(process: HANDLE, address: usize) -> String {
    let module = module_path_from_address(process, address);
    let mut line = format!("0x{:X} {}", address, module);

    if let Some(symbol) = symbol_name_from_address(process, address) {
        line.push_str(&format!(" | {symbol}"));
    }
    if let Some(source) = source_line_from_address(process, address) {
        line.push_str(&format!(" | {source}"));
    }
    line
}

fn symbol_name_from_address(process: HANDLE, address: usize) -> Option<String> {
    unsafe {
        let mut displacement = 0u64;
        let mut package = SYMBOL_INFO_PACKAGEW::default();
        package.si.SizeOfStruct = std::mem::size_of_val(&package.si) as u32;
        package.si.MaxNameLen = package.name.len() as u32;
        SymFromAddrW(process, address as u64, Some(&mut displacement), &mut package.si).ok()?;
        let name_len = package.si.NameLen as usize;
        let name = String::from_utf16_lossy(&package.name[..name_len.min(package.name.len())]);
        Some(format!("{name}+0x{displacement:X}"))
    }
}

fn source_line_from_address(process: HANDLE, address: usize) -> Option<String> {
    unsafe {
        let mut displacement = 0u32;
        let mut line = IMAGEHLP_LINEW64::default();
        line.SizeOfStruct = std::mem::size_of::<IMAGEHLP_LINEW64>() as u32;
        SymGetLineFromAddrW64(process, address as u64, &mut displacement, &mut line).ok()?;
        let file = pwstr_to_string(line.FileName.0);
        Some(format!("{}:{} +0x{:X}", file, line.LineNumber, displacement))
    }
}

fn module_path_from_address(process: HANDLE, address: usize) -> String {
    if address == 0 {
        return "<unknown>".to_string();
    }

    unsafe {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        if VirtualQueryEx(
            process,
            Some(address as *const _),
            &mut mbi,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        ) == 0
        {
            return "<unknown>".to_string();
        }

        let module = HMODULE(mbi.AllocationBase);
        if module.is_invalid() {
            return "<unknown>".to_string();
        }

        let mut buffer = vec![0u16; 1024];
        let len = K32GetModuleFileNameExW(Some(process), Some(module), &mut buffer) as usize;
        if len == 0 {
            return format!("0x{:X}", module.0 as usize);
        }
        String::from_utf16_lossy(&buffer[..len])
    }
}

fn register_dump(ctx: &CONTEXT) -> String {
    format!(
        "rip=0x{:X}\r\nrsp=0x{:X}\r\nrbp=0x{:X}\r\nrax=0x{:X}\r\nrbx=0x{:X}\r\nrcx=0x{:X}\r\nrdx=0x{:X}\r\nrsi=0x{:X}\r\nrdi=0x{:X}\r\nr8=0x{:X}\r\nr9=0x{:X}\r\nr10=0x{:X}\r\nr11=0x{:X}\r\nr12=0x{:X}\r\nr13=0x{:X}\r\nr14=0x{:X}\r\nr15=0x{:X}",
        ctx.Rip, ctx.Rsp, ctx.Rbp, ctx.Rax, ctx.Rbx, ctx.Rcx, ctx.Rdx, ctx.Rsi, ctx.Rdi, ctx.R8,
        ctx.R9, ctx.R10, ctx.R11, ctx.R12, ctx.R13, ctx.R14, ctx.R15
    )
}

fn pwstr_to_string(ptr: *mut u16) -> String {
    if ptr.is_null() {
        return "<unknown>".to_string();
    }
    unsafe {
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
}

fn write_report(path: &Path, text: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())
}

fn write_side_log(kind: &str, text: &str) -> Result<(), String> {
    let dir = PathBuf::from("logs").join("crash-reports");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!(
        "crash-logger-{kind}-{}.txt",
        Local::now().format("%Y%m%d-%H%M%S-%3f")
    ));
    write_report(&path, text)
}
