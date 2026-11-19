//! Windows 10 1809 and later.
//!
//! A pane is a pseudoconsole (ConPTY) with its program in a job object. The
//! pseudoconsole belongs to this process and cannot be handed to another, so
//! there is no live handoff here: an upgrade restarts the panes
//! (DECISIONS.md, "Windows").

use std::collections::BTreeMap;
use std::ffi::{c_void, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, FILETIME, HANDLE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    SetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACL, DACL_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, GetConsoleMode, GetStdHandle, ResizePseudoConsole,
    SetConsoleCtrlHandler, SetConsoleMode, COORD, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
    ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT, HPCON, STD_INPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
    GetExitCodeProcess, GetProcessTimes, InitializeProcThreadAttributeList, OpenEventW, OpenMutexW,
    OpenProcess, OpenProcessToken, ResumeThread, SetEvent, TerminateProcess,
    UpdateProcThreadAttribute, WaitForMultipleObjects, WaitForSingleObject,
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, INFINITE, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use super::cmdline;
use crate::pty::{ExitInfo, Signal, SpawnSpec};

/// A terminal in this process cannot be handed to the next daemon: a
/// pseudoconsole belongs to the process that made it.
pub const LOCAL_HANDOFF: bool = false;

/// So panes live in the pane host, a process that outlives the daemon.
pub const PANE_HOST: bool = true;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// A kernel handle this value closes.
struct Owned(HANDLE);
// SAFETY: a kernel handle may be used from any thread.
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: the handle is ours and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

// ---------------------------------------------------------------- terminals

struct Shared {
    /// The pseudoconsole, until it is closed (hang-up, or the program ended).
    console: Mutex<Option<HPCON>>,
    process: Owned,
    /// Every program the pane started is in this job; ending it ends them all.
    job: Owned,
    exit: Mutex<Option<ExitInfo>>,
}

impl Shared {
    /// Close the pseudoconsole: programs still on it are told to close, and
    /// the output pipe ends once what they wrote has been read.
    fn close_console(self: &Arc<Self>) {
        let taken = self.console.lock().unwrap().take();
        if let Some(hpc) = taken {
            // Before Windows 11 24H2 this waits for the console host to
            // finish writing, so it is never done on the caller's thread.
            std::thread::spawn(move || {
                // SAFETY: the handle was taken out of the mutex, so it is closed once.
                unsafe { ClosePseudoConsole(hpc) };
            });
        }
    }

    fn collect_exit(&self) -> ExitInfo {
        let mut code = 0u32;
        // SAFETY: a process handle we own and a valid pointer.
        let ok = unsafe { GetExitCodeProcess(self.process.0, &mut code) };
        let info = if ok != 0 {
            ExitInfo::Code(code as i32)
        } else {
            ExitInfo::Unknown
        };
        *self.exit.lock().unwrap() = Some(info);
        info
    }
}

pub struct Pty {
    shared: Arc<Shared>,
    pid: i32,
    input: File,
    output: File,
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pty(pid {})", self.pid)
    }
}

fn coord(cols: u16, rows: u16) -> COORD {
    COORD {
        X: cols.clamp(2, i16::MAX as u16) as i16,
        Y: rows.clamp(1, i16::MAX as u16) as i16,
    }
}

fn hresult(hr: i32) -> io::Result<()> {
    if hr >= 0 {
        Ok(())
    } else {
        // An HRESULT made from a Win32 error carries it in the low 16 bits.
        Err(io::Error::from_raw_os_error(hr & 0xFFFF))
    }
}

fn pipe() -> io::Result<(Owned, Owned)> {
    let (mut read, mut write): (HANDLE, HANDLE) = (null_mut(), null_mut());
    // SAFETY: valid out pointers; no security attributes, so not inheritable.
    if unsafe { CreatePipe(&mut read, &mut write, null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((Owned(read), Owned(write)))
}

fn into_file(h: Owned) -> File {
    let raw = h.0;
    std::mem::forget(h);
    // SAFETY: ownership of an open handle moves into the File.
    File::from(unsafe { OwnedHandle::from_raw_handle(raw as _) })
}

/// The environment a pane's program starts with, as Windows wants it: names
/// compared without regard to case, sorted, each `NAME=value` ending in a
/// zero, and one more zero at the end.
fn environment(spec: &SpawnSpec) -> (Vec<u16>, BTreeMap<String, OsString>) {
    let mut vars: BTreeMap<String, (OsString, OsString)> = BTreeMap::new();
    for (k, v) in std::env::vars_os() {
        vars.insert(k.to_string_lossy().to_uppercase(), (k, v));
    }
    for k in &spec.env_remove {
        vars.remove(&k.to_uppercase());
    }
    let mut set = |k: &str, v: &str| {
        vars.insert(k.to_uppercase(), (k.into(), v.into()));
    };
    set("TERM", "xterm-256color");
    set("COLORTERM", "truecolor");
    for (k, v) in &spec.env {
        set(k, v);
    }
    let mut block = vec![];
    let mut by_name = BTreeMap::new();
    for (upper, (k, v)) in vars {
        block.extend(k.encode_wide());
        block.push('=' as u16);
        block.extend(v.encode_wide());
        block.push(0);
        by_name.insert(upper, v);
    }
    block.push(0);
    (block, by_name)
}

/// The file a program name stands for: as given when it names a folder,
/// else the first match on PATH, trying each `PATHEXT` extension.
fn resolve(program: &str, env: &BTreeMap<String, OsString>, cwd: Option<&Path>) -> Option<PathBuf> {
    let pathext = env
        .get("PATHEXT")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| cmdline::DEFAULT_PATHEXT.to_string());
    let names = cmdline::names_for(program, &pathext);
    let has_dir = program.contains(['\\', '/']) || Path::new(program).is_absolute();
    if has_dir {
        return std::iter::once(program.to_string())
            .chain(names)
            .map(|n| match cwd {
                Some(dir) => dir.join(n),
                None => PathBuf::from(n),
            })
            .find(|p| p.is_file());
    }
    let path = env.get("PATH").cloned().unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|d| !d.as_os_str().is_empty())
        .flat_map(|d| names.iter().map(move |n| d.join(n)).collect::<Vec<_>>())
        .find(|p| p.is_file())
}

impl Pty {
    pub fn spawn(spec: &SpawnSpec) -> io::Result<Pty> {
        let (env_block, env) = environment(spec);
        let found = resolve(&spec.program, &env, spec.cwd.as_deref()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{}: no such program", spec.program),
            )
        })?;
        let found_text = found.display().to_string();
        // A .cmd or .bat file is not a program: cmd.exe runs it.
        let (application, line) = if cmdline::is_script(&found_text) {
            let cmd = env
                .get("COMSPEC")
                .map(PathBuf::from)
                .filter(|p| p.is_file())
                .or_else(|| resolve("cmd.exe", &env, None))
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cmd.exe was not found"))?;
            let line = cmdline::script_line(&found_text, &spec.args)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            (cmd, line)
        } else {
            let line = cmdline::program_line(&found_text, &spec.args);
            (found, line)
        };

        let (in_read, in_write) = pipe()?;
        let (out_read, out_write) = pipe()?;
        let mut hpc: HPCON = 0;
        // SAFETY: both pipe ends are open; `hpc` is a valid out pointer.
        hresult(unsafe {
            CreatePseudoConsole(
                coord(spec.cols, spec.rows),
                in_read.0,
                out_write.0,
                0,
                &mut hpc,
            )
        })?;
        // The pseudoconsole holds its own copies of these two ends.
        drop(in_read);
        drop(out_write);
        // From here the pseudoconsole must be closed on every way out.
        struct Console(Option<HPCON>);
        impl Drop for Console {
            fn drop(&mut self) {
                if let Some(h) = self.0.take() {
                    // SAFETY: created above, closed once.
                    unsafe { ClosePseudoConsole(h) };
                }
            }
        }
        let mut console = Console(Some(hpc));

        // The one attribute: this program's console is the pseudoconsole.
        let mut size = 0usize;
        // SAFETY: the documented two-call pattern; the first call reports the size.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size) };
        let mut attrs = vec![0usize; size.div_ceil(std::mem::size_of::<usize>()).max(1)];
        let list = attrs.as_mut_ptr() as *mut c_void;
        // SAFETY: `attrs` is at least `size` bytes and outlives the list.
        if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        struct List(*mut c_void);
        impl Drop for List {
            fn drop(&mut self) {
                // SAFETY: initialized above.
                unsafe { DeleteProcThreadAttributeList(self.0) };
            }
        }
        let _list = List(list);
        // SAFETY: for this attribute the value is the HPCON itself, by value.
        let ok = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc as *const c_void,
                std::mem::size_of::<HPCON>(),
                null_mut(),
                null(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: a plain C struct for which all-zero is a valid start.
        let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // With no standard handles named, the program takes the
        // pseudoconsole's, not ones this daemon happened to be started with.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = list;
        // SAFETY: as above.
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let app = wide(application.as_os_str());
        let mut line = wide(OsStr::new(&line));
        let cwd = spec.cwd.as_ref().map(|c| wide(c.as_os_str()));
        // SAFETY: every pointer is to a live, zero-terminated buffer; the
        // command line buffer is writable as CreateProcessW requires.
        let ok = unsafe {
            CreateProcessW(
                app.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
                env_block.as_ptr() as *const c_void,
                cwd.as_ref().map_or(null(), |c| c.as_ptr()),
                &si.StartupInfo,
                &mut pi,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let process = Owned(pi.hProcess);
        let thread = Owned(pi.hThread);

        // The job: the program is put in it before it runs a single
        // instruction, so everything it ever starts is in it too.
        let end = |e: io::Error| {
            // SAFETY: the process handle is ours.
            unsafe { TerminateProcess(process.0, 1) };
            e
        };
        // SAFETY: no name and no attributes; the handle is checked below.
        let job = Owned(unsafe { CreateJobObjectW(null(), null()) });
        if job.0.is_null() {
            return Err(end(io::Error::last_os_error()));
        }
        // SAFETY: a plain C struct for which all-zero is valid.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the struct matches the information class and its size.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0
                && AssignProcessToJobObject(job.0, process.0) != 0
        };
        if !ok {
            return Err(end(io::Error::last_os_error()));
        }
        // SAFETY: the thread handle of the process created suspended above.
        unsafe { ResumeThread(thread.0) };
        drop(thread);

        let shared = Arc::new(Shared {
            console: Mutex::new(console.0.take()),
            process,
            job,
            exit: Mutex::new(None),
        });
        // The pseudoconsole does not end its output when the program ends:
        // it is closed here, and that is what the pane's reader sees as the end.
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name(format!("pane-wait-{}", pi.dwProcessId))
                .spawn(move || {
                    // SAFETY: a process handle the Arc keeps open.
                    unsafe { WaitForSingleObject(shared.process.0, INFINITE) };
                    shared.collect_exit();
                    shared.close_console();
                })?;
        }
        Ok(Pty {
            shared,
            pid: pi.dwProcessId as i32,
            input: into_file(in_write),
            output: into_file(out_read),
        })
    }

    /// There is nothing to adopt on Windows: a pseudoconsole cannot change hands.
    ///
    /// # Safety
    /// Always safe here; unsafe to match the Unix signature.
    pub unsafe fn adopt(_fd: i32, _pid: i32) -> io::Result<Pty> {
        Err(no_handoff())
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    pub fn reader(&self) -> io::Result<PtyReader> {
        Ok(PtyReader {
            file: self.output.try_clone()?,
            shared: self.shared.clone(),
        })
    }

    pub fn writer(&self) -> io::Result<PtyWriter> {
        Ok(PtyWriter(self.input.try_clone()?))
    }

    pub fn handoff_fd(&self) -> io::Result<i32> {
        Err(no_handoff())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        match *self.shared.console.lock().unwrap() {
            // SAFETY: the handle is open while it is in the mutex, which is held.
            Some(hpc) => hresult(unsafe { ResizePseudoConsole(hpc, coord(cols, rows)) }),
            // Closed already: the program is ending, there is nothing to size.
            None => Ok(()),
        }
    }

    /// Tell every program in the pane. See `Signal` for what each means here.
    pub fn signal(&self, sig: Signal) -> io::Result<()> {
        match sig {
            Signal::Hangup => {
                self.shared.close_console();
                Ok(())
            }
            Signal::Terminate | Signal::Kill => {
                // SAFETY: a job handle we own.
                if unsafe { TerminateJobObject(self.shared.job.0, 1) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            }
        }
    }

    pub fn try_wait(&self) -> Option<ExitInfo> {
        // SAFETY: a process handle we own.
        match unsafe { WaitForSingleObject(self.shared.process.0, 0) } {
            WAIT_OBJECT_0 => Some(self.shared.collect_exit()),
            _ => None,
        }
    }

    pub fn wait(&self) -> ExitInfo {
        // SAFETY: a process handle we own.
        unsafe { WaitForSingleObject(self.shared.process.0, INFINITE) };
        self.shared.collect_exit()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // A pane that is forgotten takes its programs with it. The thread
        // waiting on the process still holds the handles; it lets go when
        // the process has ended.
        // SAFETY: a job handle we own.
        unsafe { TerminateJobObject(self.shared.job.0, 1) };
        self.shared.close_console();
    }
}

fn no_handoff() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "a terminal cannot be handed to another process on Windows",
    )
}

pub struct PtyReader {
    file: File,
    shared: Arc<Shared>,
}

impl PtyReader {
    /// There is no cheap wait on this pipe, and nothing on Windows asks a
    /// reader to stop while the terminal stays open (there is no handoff).
    pub fn ready(&mut self) -> io::Result<bool> {
        Ok(true)
    }

    /// Read output. `Ok(0)`: the pseudoconsole was closed and is drained.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }

    /// After the output ended: how the pane's program ended.
    pub fn exit_status(&self) -> ExitInfo {
        if let Some(info) = *self.shared.exit.lock().unwrap() {
            return info;
        }
        // Hung up, and the program has not gone yet: give it a moment.
        // SAFETY: a process handle the Arc keeps open.
        match unsafe { WaitForSingleObject(self.shared.process.0, 2000) } {
            WAIT_OBJECT_0 => self.shared.collect_exit(),
            _ => ExitInfo::Unknown,
        }
    }
}

pub struct PtyWriter(File);

impl Write for PtyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn open_process(pid: u32) -> Option<Owned> {
    // SAFETY: plain call; a null result is checked.
    let h = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    (!h.is_null()).then_some(Owned(h))
}

/// Is a process with this pid still alive?
pub fn pid_alive(pid: i32) -> bool {
    // SAFETY: a handle just opened.
    open_process(pid as u32).is_some_and(|h| unsafe { WaitForSingleObject(h.0, 0) } == WAIT_TIMEOUT)
}

// --------------------------------------------------------------- this user

/// A security descriptor made from its text form, freed when dropped.
struct Descriptor(PSECURITY_DESCRIPTOR);
// SAFETY: a block of memory that is only read after it is made.
unsafe impl Send for Descriptor {}
unsafe impl Sync for Descriptor {}
impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by the system with LocalAlloc.
        unsafe { LocalFree(self.0 as _) };
    }
}

impl Descriptor {
    fn from_sddl(sddl: &str) -> io::Result<Descriptor> {
        let text = wide(OsStr::new(sddl));
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: zero-terminated text and a valid out pointer. Revision 1.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Descriptor(sd))
    }
}

/// The user this process runs as: the raw SID, and its text form (`S-1-5-…`).
fn current_user() -> io::Result<(Vec<u8>, String)> {
    let mut token: HANDLE = null_mut();
    // SAFETY: valid out pointer; the pseudo-handle needs no closing.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Owned(token);
    let mut len = 0u32;
    // SAFETY: the first call reports the size needed.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut len) };
    // u64 storage so the TOKEN_USER at its start is aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: the buffer is at least `len` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr() as *mut c_void,
            len,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the system wrote a TOKEN_USER at the start of the buffer.
    let sid = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
    let mut text: *mut u16 = null_mut();
    // SAFETY: a valid SID and out pointer.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the system returned a zero-terminated string; it is freed below.
    let name = unsafe {
        let mut n = 0;
        while *text.add(n) != 0 {
            n += 1;
        }
        let s = OsString::from_wide(std::slice::from_raw_parts(text, n));
        LocalFree(text as _);
        s.to_string_lossy().into_owned()
    };
    // SAFETY: a SID is 8 bytes plus 4 for each sub-authority (its 2nd byte).
    let raw = unsafe {
        let count = *(sid as *const u8).add(1) as usize;
        std::slice::from_raw_parts(sid as *const u8, 8 + 4 * count).to_vec()
    };
    Ok((raw, name))
}

// ------------------------------------------------------------- local socket

/// The pipe the CLI, hooks and `spandeck mcp` reach the daemon through: a
/// named pipe only the daemon's own user can open.
pub mod local {
    use std::ffi::{c_void, OsStr};
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::ptr::null_mut;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::{LocalFree, ERROR_PIPE_BUSY};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::{
        EqualSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    };
    use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    use super::{current_user, Descriptor};

    /// One accepted connection. Split it with `tokio::io::split`.
    pub type Stream = NamedPipeServer;

    pub struct Listener {
        name: PathBuf,
        /// Owner and the only account allowed: this user.
        descriptor: Descriptor,
        /// The instance the next client connects to.
        next: NamedPipeServer,
    }

    fn instance(name: &Path, descriptor: &Descriptor, first: bool) -> io::Result<NamedPipeServer> {
        let mut attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        // SAFETY: `attrs` points at a valid descriptor for the length of the call.
        unsafe {
            ServerOptions::new()
                // Fails if anyone already made a pipe with this name.
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, &mut attrs as *mut _ as *mut c_void)
        }
    }

    impl Listener {
        /// Listen at `path` (a `\\.\pipe\…` name). Fails if another process
        /// already listens there.
        pub fn bind(path: &Path) -> io::Result<Listener> {
            let (_, sid) = current_user()?;
            let descriptor = Descriptor::from_sddl(&format!("O:{sid}D:P(A;;GA;;;{sid})"))?;
            let next = instance(path, &descriptor, true)?;
            Ok(Listener {
                name: path.to_path_buf(),
                descriptor,
                next,
            })
        }

        pub async fn accept(&mut self) -> io::Result<Stream> {
            self.next.connect().await?;
            // The following instance exists before this one is handed out, so
            // there is never a moment with no pipe for a client to open.
            let following = instance(&self.name, &self.descriptor, false)?;
            Ok(std::mem::replace(&mut self.next, following))
        }
    }

    /// A blocking connection, for the CLI and hooks.
    pub struct Conn {
        file: File,
        timeout: Arc<Mutex<Option<Duration>>>,
    }

    /// Is the pipe behind this handle owned by the user we run as?
    fn owned_by_us(file: &File) -> io::Result<bool> {
        let (me, _) = current_user()?;
        let mut owner: PSID = null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: an open handle and valid out pointers; `sd` is freed below.
        let err = unsafe {
            GetSecurityInfo(
                file.as_raw_handle() as _,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                null_mut(),
                null_mut(),
                &mut sd,
            )
        };
        if err != 0 {
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        // SAFETY: both are valid SIDs; `owner` points into `sd`.
        let same = unsafe {
            let same = !owner.is_null() && EqualSid(owner, me.as_ptr() as PSID) != 0;
            LocalFree(sd as _);
            same
        };
        Ok(same)
    }

    pub fn connect(path: &Path) -> io::Result<Conn> {
        let start = Instant::now();
        let file = loop {
            let opened = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                // The server may learn who we are, and may not act as us.
                .security_qos_flags(SECURITY_IDENTIFICATION)
                .open(path);
            match opened {
                Ok(f) => break f,
                // Every instance is taken for a moment: the daemon makes the
                // next one as soon as it accepts.
                Err(e)
                    if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
                        && start.elapsed() < Duration::from_secs(2) =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        };
        // A pipe someone else made first under this name gets nothing from us.
        if !owned_by_us(&file)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the pipe belongs to another user",
            ));
        }
        Ok(Conn {
            file,
            timeout: Default::default(),
        })
    }

    impl Conn {
        pub fn try_clone(&self) -> io::Result<Conn> {
            Ok(Conn {
                file: self.file.try_clone()?,
                timeout: self.timeout.clone(),
            })
        }

        pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
            *self.timeout.lock().unwrap() = d;
            Ok(())
        }
    }

    impl Read for Conn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let limit = *self.timeout.lock().unwrap();
            if let Some(limit) = limit {
                // A pipe opened for plain reads has no timeout of its own:
                // look until something is there.
                let start = Instant::now();
                loop {
                    let mut waiting = 0u32;
                    // SAFETY: an open pipe handle; only the count is asked for.
                    let ok = unsafe {
                        PeekNamedPipe(
                            self.file.as_raw_handle() as _,
                            null_mut(),
                            0,
                            null_mut(),
                            &mut waiting,
                            null_mut(),
                        )
                    };
                    // Closed by the other end: the read below reports it.
                    if ok == 0 || waiting > 0 {
                        break;
                    }
                    if start.elapsed() >= limit {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            self.file.read(buf)
        }
    }

    impl Write for Conn {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.file.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The pipe's name for a station whose state folder hashes to `tag`.
    pub fn pipe_name(tag: &str) -> Option<PathBuf> {
        Some(PathBuf::from(
            OsStr::new(&format!(r"\\.\pipe\spandeck-{tag}")).to_os_string(),
        ))
    }
}

// -------------------------------------------------------------------- files

/// Write a file only this user can read. On Windows that comes from the
/// folder: `private_dir` gives the state and config folders an access list
/// that files made in them inherit.
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(contents)?;
    f.sync_all()
}

/// Give a folder, and everything made in it from now on, to this user,
/// SYSTEM and Administrators only, with nothing inherited from its parent.
pub fn private_dir(path: &Path) -> io::Result<()> {
    let (_, sid) = current_user()?;
    let descriptor = Descriptor::from_sddl(&format!(
        "D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
    ))?;
    let (mut present, mut defaulted) = (0, 0);
    let mut dacl: *mut ACL = null_mut();
    // SAFETY: a valid descriptor and out pointers; `dacl` points into it.
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    let name = wide(path.as_os_str());
    // SAFETY: a zero-terminated path and an access list that outlives the call.
    let err = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null(),
        )
    };
    if err != 0 {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    Ok(())
}

fn pathext() -> String {
    std::env::var("PATHEXT").unwrap_or_else(|_| cmdline::DEFAULT_PATHEXT.to_string())
}

/// Is this a file the system would run? There is no executable bit: it is
/// one whose extension is in `PATHEXT`.
pub fn is_executable(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    path.is_file()
        && pathext()
            .split(';')
            .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext))
}

/// Nothing to mark: a file is a program by its extension.
pub fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Windows has no permission bits; who may read a file is its folder's
/// access list (`private_dir`).
pub fn file_mode(_path: &Path) -> Option<u32> {
    None
}

/// The file names a command name can stand for (`claude`: `claude.exe`,
/// `claude.cmd`, …).
pub fn executable_names(name: &str) -> Vec<String> {
    cmdline::names_for(name, &pathext())
}

/// Where state and config are kept when the environment does not say.
pub fn default_dirs(home: &Path) -> (PathBuf, PathBuf) {
    let folder = |var: &str, fallback: &str| {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(fallback))
            .join("spandeck")
    };
    (
        folder("LOCALAPPDATA", r"AppData\Local"),
        folder("APPDATA", r"AppData\Roaming"),
    )
}

/// Put `new` where `to` is, although `to` may be a program that is running:
/// a running program's file cannot be written over, but it can be renamed.
/// The file moved aside is `<to>.old`; when a program that is still running
/// from an earlier replacement holds that name (the supervisor, the pane
/// host), the next free of `.old2` to `.old9`. Each is removed as soon as
/// nothing runs from it.
pub fn replace_file(new: &Path, to: &Path) -> io::Result<()> {
    let aside = |n: u32| {
        let mut old = to.as_os_str().to_os_string();
        old.push(if n == 1 {
            ".old".to_string()
        } else {
            format!(".old{n}")
        });
        PathBuf::from(old)
    };
    let mut free = None;
    for n in 1..=9 {
        let old = aside(n);
        // Left by an earlier replacement; gone unless that program still runs.
        let _ = std::fs::remove_file(&old);
        if free.is_none() && !old.exists() {
            free = Some(old);
        }
    }
    let old = free.unwrap_or_else(|| aside(1));
    if to.exists() {
        std::fs::rename(to, &old)?;
    }
    match std::fs::rename(new, to) {
        Ok(()) => {
            let _ = std::fs::remove_file(&old);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::rename(&old, to);
            Err(e)
        }
    }
}

// ---------------------------------------------------------------- processes

/// Where `replace_process` writes the pid of the daemon it starts: the
/// station's pid file. The supervisor follows the station by that file, and
/// the daemon that takes over writes it itself only once it has waited for
/// this one to end.
static SUCCESSOR_PID_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Name the file `replace_process` records the next daemon's pid in.
pub fn record_successor_in(pid_file: &Path) {
    if let Ok(mut f) = SUCCESSOR_PID_FILE.lock() {
        *f = Some(pid_file.to_path_buf());
    }
}

/// Start `cmd` as the daemon that takes over from this one, then end this
/// process. The panes are in the pane host and stay as they are. The new
/// daemon is told to wait for this process to be gone (`SPANDECK_WAIT_PID`)
/// before it opens the pipe and the database, and its pid is written to the
/// station's pid file before this process ends, so whoever watches this one
/// end finds the next one there. Returns only if the new program could not
/// be started.
pub fn replace_process(cmd: &mut Command) -> io::Error {
    cmd.env("SPANDECK_WAIT_PID", std::process::id().to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    match cmd.spawn() {
        Ok(next) => {
            let file = SUCCESSOR_PID_FILE.lock().ok().and_then(|f| f.clone());
            if let Some(file) = file {
                let _ = std::fs::write(file, next.id().to_string());
            }
            std::process::exit(0)
        }
        Err(e) => e,
    }
}

/// Start a program that goes on after this one ends, with no console: out
/// of this process's job where that is allowed (the Task Scheduler runs a
/// task in one), so ending the daemon's task does not end it.
pub fn spawn_detached(cmd: &mut Command) -> io::Result<()> {
    use windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    cmd.creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB);
    if cmd.spawn().is_ok() {
        return Ok(());
    }
    // A job that does not allow leaving it refuses the flag.
    cmd.creation_flags(flags);
    cmd.spawn().map(|_| ())
}

/// The upgraded daemon is a new process.
pub const UPGRADE_KEEPS_PID: bool = false;

/// Nothing to do: a pane's program is in a job that ended with the process
/// that started it.
pub fn end_leftover_child(_pid: i32) {}

/// A process, as `processes` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: i32,
    pub sid: i32,
    pub age_secs: u64,
    pub name: String,
}

/// Nothing is listed: a pane's programs are in a job object, which ends
/// every one of them with the pane, so none is ever left over.
pub fn processes() -> Vec<Proc> {
    vec![]
}

/// Nothing to do: see `processes`.
pub fn end_processes(_pids: &[i32]) {}

/// What one pane's programs have used and hold now (`tree_usage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TreeUse {
    pub cpu_ms: u64,
    pub rss_bytes: u64,
    pub count: u32,
}

/// Not read on Windows, so nothing is shown there. A pane's programs are
/// in a job object, whose accounting would say this soundly, but the job
/// is held by the pane host's process and this process has only the
/// program's id: the host would have to be asked, which it cannot be yet.
pub fn tree_usage(_roots: &[i32]) -> Option<Vec<TreeUse>> {
    None
}

/// Nothing to do: handles are not inherited unless asked for.
pub fn close_inherited() {}

/// Wait for another process to end (the daemon this one takes over from).
pub fn wait_for_exit(pid: u32, limit: Duration) {
    if let Some(h) = open_process(pid) {
        // SAFETY: a handle just opened.
        unsafe { WaitForSingleObject(h.0, limit.as_millis().min(u32::MAX as u128 - 1) as u32) };
    }
}

/// Resolves when the daemon is asked to stop: Ctrl-C or Ctrl-Break in its
/// console, the console closing, log off, shut down, or `request_stop` for
/// this station (`spandeck service --stop`), which is what SIGTERM is on
/// Unix: a daemon with no console can be sent none of the others.
pub async fn shutdown_signal(station: &Path) -> io::Result<()> {
    use tokio::signal::windows;
    let mut c = windows::ctrl_c()?;
    let mut brk = windows::ctrl_break()?;
    let mut close = windows::ctrl_close()?;
    let mut logoff = windows::ctrl_logoff()?;
    let mut shutdown = windows::ctrl_shutdown()?;
    let (tx, asked) = tokio::sync::oneshot::channel::<()>();
    if let Some(event) = named(station, "stop", |name| {
        // SAFETY: a valid name; a null result is checked by `named`.
        unsafe { CreateEventW(null(), 1, 0, name) }
    }) {
        std::thread::spawn(move || {
            // The whole value moves here, not its field: it is what may
            // cross threads, and it closes the handle when this ends.
            let event = event;
            // SAFETY: the handle is owned by this thread until it ends.
            if unsafe { WaitForSingleObject(event.0, INFINITE) } == WAIT_OBJECT_0 {
                let _ = tx.send(());
            }
        });
    }
    tokio::select! {
        _ = c.recv() => {}
        _ = brk.recv() => {}
        _ = close.recv() => {}
        _ = logoff.recv() => {}
        _ = shutdown.recv() => {}
        Ok(()) = asked => {}
    }
    Ok(())
}

/// Ask the daemon of this station to stop, as its owner. False when no
/// daemon of this station is listening for it.
pub fn request_stop(station: &Path) -> io::Result<bool> {
    let Some(event) = named(station, "stop", |name| {
        // SAFETY: a valid name; a null result is checked by `named`.
        unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name) }
    }) else {
        return Ok(false);
    };
    // SAFETY: a handle just opened.
    if unsafe { SetEvent(event.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(true)
}

// ------------------------------------------------ supervising the station
//
// The log-on task's program (`spandeckd.exe`) keeps the station running: it
// follows whichever daemon is current, which after an upgrade is not one it
// started (DECISIONS.md, "Windows"). What it needs of the system is here.

const EVENT_MODIFY_STATE: u32 = 0x0002;
const SYNCHRONIZE: u32 = 0x0010_0000;

/// Open or make a kernel object named for this station (`station` is its
/// local socket, whose last part is one name per state folder). Tried in the
/// namespace every session of the machine shares, so that a command typed
/// over SSH reaches the station in the owner's desktop session, then in
/// this session's own. The object is this user's: its access list is the
/// default one, the user and SYSTEM.
fn named(station: &Path, what: &str, open: impl Fn(*const u16) -> HANDLE) -> Option<Owned> {
    named_new(station, what, open).map(|(h, _)| h)
}

/// `named`, saying also whether the object was there before a call that
/// makes one.
fn named_new(
    station: &Path,
    what: &str,
    open: impl Fn(*const u16) -> HANDLE,
) -> Option<(Owned, bool)> {
    let text = station.to_string_lossy();
    let leaf: String = text
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    for space in ["Global", "Local"] {
        let name = wide(OsStr::new(&format!(r"{space}\{leaf}-{what}")));
        let h = open(name.as_ptr());
        // SAFETY: plain call, read at once after the call it is about.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if !h.is_null() {
            return Some((Owned(h), existed));
        }
    }
    None
}

/// The claim that makes this process the one supervisor of a station, and
/// the way the next supervisor asks it to leave.
pub struct Supervisor {
    _claim: Owned,
    leave: Owned,
}

/// What `Supervisor::watch` saw.
#[derive(Debug, PartialEq, Eq)]
pub enum Watched {
    /// The process ended, with this exit code.
    Ended(i32),
    /// No such process (it ended before it could be watched).
    Gone,
    /// Another supervisor is taking over: this one is to end.
    Leave,
}

/// Become the supervisor of a station. None when another process already
/// is. With `take_over`, the one that is supervising is asked to leave and
/// waited for that long: how a newly installed supervisor replaces the one
/// that is running, whose file Windows would not let be written over.
pub fn supervise(station: &Path, take_over: Duration) -> Option<Supervisor> {
    let deadline = std::time::Instant::now() + take_over;
    loop {
        // The claim is the object's existence: it goes when the process
        // that made it ends, however that happens. One that was there
        // already is somebody else's, and is let go of at once.
        let claim = named_new(station, "supervisor", |name| {
            // SAFETY: a valid name; a null result is checked by `named_new`.
            unsafe { CreateMutexW(null(), 0, name) }
        });
        if let Some((claim, false)) = claim {
            let leave = named(station, "leave", |name| {
                // SAFETY: a valid name; a null result is checked by `named`.
                unsafe { CreateEventW(null(), 1, 0, name) }
            })?;
            return Some(Supervisor {
                _claim: claim,
                leave,
            });
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        if let Some(leave) = named(station, "leave", |name| {
            // SAFETY: a valid name; a null result is checked by `named`.
            unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name) }
        }) {
            // SAFETY: a handle just opened.
            unsafe { SetEvent(leave.0) };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Does some process supervise this station now?
pub fn supervised(station: &Path) -> bool {
    named(station, "supervisor", |name| {
        // SAFETY: a valid name; a null result is checked by `named`.
        unsafe { OpenMutexW(SYNCHRONIZE, 0, name) }
    })
    .is_some()
}

impl Supervisor {
    /// Wait until the process ends, or until this supervisor is asked to
    /// leave.
    pub fn watch(&self, pid: u32) -> Watched {
        let Some(process) = open_process(pid) else {
            return Watched::Gone;
        };
        let both = [process.0, self.leave.0];
        // SAFETY: two handles that are open for the length of the call.
        let woke = unsafe { WaitForMultipleObjects(2, both.as_ptr(), 0, INFINITE) };
        if woke != WAIT_OBJECT_0 {
            return Watched::Leave;
        }
        let mut code = 0u32;
        // SAFETY: an open handle and a valid out pointer.
        unsafe { GetExitCodeProcess(process.0, &mut code) };
        Watched::Ended(code as i32)
    }

    /// Wait this long. True when asked to leave in that time.
    pub fn leave_within(&self, wait: Duration) -> bool {
        let ms = wait.as_millis().min(u32::MAX as u128 - 1) as u32;
        // SAFETY: an open handle.
        unsafe { WaitForSingleObject(self.leave.0, ms) == WAIT_OBJECT_0 }
    }
}

/// When a process that is running now was started. None when there is no
/// such process, or it has ended.
pub fn process_started(pid: u32) -> Option<std::time::SystemTime> {
    let process = open_process(pid)?;
    // SAFETY: a handle just opened.
    if unsafe { WaitForSingleObject(process.0, 0) } != WAIT_TIMEOUT {
        return None;
    }
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut made, mut ended, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: an open handle and four valid out pointers.
    if unsafe { GetProcessTimes(process.0, &mut made, &mut ended, &mut kernel, &mut user) } == 0 {
        return None;
    }
    // 100 ns steps since 1601; the Unix epoch is 11 644 473 600 s later.
    let ticks = (u64::from(made.dwHighDateTime) << 32) | u64::from(made.dwLowDateTime);
    let since_1970 = ticks.checked_sub(116_444_736_000_000_000)?;
    Some(std::time::UNIX_EPOCH + Duration::from_nanos(since_1970.checked_mul(100)?))
}

/// Start `cmd` without opening a console window.
pub fn no_window(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe extern "system" fn note_interrupt(_kind: u32) -> i32 {
    INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
    // Handled: this process goes on. Programs it started get the same
    // Ctrl-C and decide for themselves (a handler, unlike "ignore", is not
    // passed on to them).
    1
}

/// Let Ctrl-C reach the programs this one starts without ending this one.
pub fn ignore_interrupt() {
    // SAFETY: the handler is a plain function that lives as long as the program.
    unsafe { SetConsoleCtrlHandler(Some(note_interrupt), 1) };
}

/// Was Ctrl-C pressed since `ignore_interrupt`?
pub fn interrupted() -> bool {
    INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst)
}

// -------------------------------------------------------------- the machine

/// The computer's name, in lower case as Tailscale and DNS show it.
pub fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|n| !n.is_empty())
        .map(|n| n.to_lowercase())
}

/// Windows keeps no load average.
pub fn load_average() -> Option<f64> {
    None
}

/// Fill `buf` from the system's random source.
pub fn random_bytes(buf: &mut [u8]) -> io::Result<()> {
    use windows_sys::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    // SAFETY: a valid buffer of the stated length; no algorithm handle is
    // needed with the system-preferred flag.
    let status = unsafe {
        BCryptGenRandom(
            null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        return Err(io::Error::other(format!(
            "the system random source failed ({status:#x})"
        )));
    }
    Ok(())
}

/// The command that holds the machine awake while it runs: spandeck itself,
/// which ends when this daemon does.
pub fn keep_awake_cmd(exe: &Path) -> Vec<String> {
    vec![
        exe.display().to_string(),
        "keep-awake".to_string(),
        std::process::id().to_string(),
    ]
}

/// Hold the machine awake until the process `while_pid` ends, or for as
/// long as this process runs if it cannot be watched.
pub fn hold_awake(while_pid: u32) -> io::Result<()> {
    // SAFETY: plain call. The request lasts as long as this thread.
    if unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) } == 0 {
        return Err(io::Error::other("Windows refused the keep-awake request"));
    }
    match open_process(while_pid) {
        // SAFETY: a handle just opened.
        Some(h) => unsafe {
            WaitForSingleObject(h.0, INFINITE);
        },
        None => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
    }
    // SAFETY: plain call; clears the request.
    unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
    Ok(())
}

// ------------------------------------------------------------------- shells

fn on_path(name: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|d| d.join(name).is_file())
}

/// The shell a plain pane runs: PowerShell 7, else Windows PowerShell, else cmd.
pub fn default_shell() -> (String, Vec<String>) {
    for (name, args) in [
        ("pwsh.exe", &["-NoLogo"][..]),
        ("powershell.exe", &["-NoLogo"][..]),
    ] {
        if on_path(name) {
            return (
                name.to_string(),
                args.iter().map(|a| a.to_string()).collect(),
            );
        }
    }
    (
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into()),
        vec![],
    )
}

/// The command that runs one shell line: Windows PowerShell, which every
/// Windows has and the vendors' install lines are written for.
pub fn shell_line(line: &str) -> Vec<String> {
    [
        "powershell.exe",
        "-NoLogo",
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        line,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

// ---------------------------------------------------------------- a console

/// Standard input as it was before `stdin_raw`; restores it when dropped.
pub struct RawInput(Option<u32>);

/// Read standard input a key at a time, with no echo and Ctrl-C as a byte.
pub fn stdin_raw() -> RawInput {
    // SAFETY: plain console calls with a valid out pointer.
    unsafe {
        let h = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(h, &mut mode) == 0 {
            return RawInput(None);
        }
        let raw = (mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
            | ENABLE_VIRTUAL_TERMINAL_INPUT;
        SetConsoleMode(h, raw);
        RawInput(Some(mode))
    }
}

impl RawInput {
    pub fn restore(&mut self) {
        if let Some(mode) = self.0.take() {
            // SAFETY: restoring the mode read from the same handle.
            unsafe { SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), mode) };
        }
    }
}

impl Drop for RawInput {
    fn drop(&mut self) {
        self.restore();
    }
}

// ------------------------------------------------------- who, and whose file

/// The name of the user this process runs as.
pub fn user_name() -> Option<String> {
    std::env::var("USERNAME").ok().filter(|u| !u.is_empty())
}

/// Nothing is run with an administrator's right on Windows (https.rs).
pub fn system_program(_path: &Path) -> bool {
    false
}
