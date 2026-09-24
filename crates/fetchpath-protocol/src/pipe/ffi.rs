//! The Win32 calls the pipe transport makes, each wrapped so the rest of the
//! module is safe code.
//!
//! Every handle is owned by a [`Handle`] that closes it once. Every
//! overlapped operation waits for completion (after cancelling it on timeout)
//! before its `OVERLAPPED` and buffer go out of scope, so the kernel never
//! writes into freed memory.

use std::io;
use std::ptr;
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_FLAGS_AND_ATTRIBUTES, FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, INFINITE, OpenProcessToken,
};

/// `ERROR_SEM_TIMEOUT`, which `WaitNamedPipeW` reports when no instance
/// became free in time.
const ERROR_SEM_TIMEOUT: u32 = 121;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PIPE_BUSY: u32 = 231;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_OPERATION_ABORTED: u32 = 995;
const ERROR_IO_INCOMPLETE: u32 = 996;

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// An owned kernel handle.
pub struct Handle(HANDLE);

// SAFETY: a file or pipe handle may be used from any thread. Concurrent
// overlapped operations are safe because each one has its own OVERLAPPED.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    fn from_raw(raw: HANDLE) -> io::Result<Self> {
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(raw))
        }
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// A security descriptor built from SDDL, freed with `LocalFree`.
pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(sddl);
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `text` is NUL-terminated and outlives the call; the
        // descriptor is written to a local and owned afterwards.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    /// Attributes that point at this descriptor. Valid while `self` lives.
    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

/// A handle's owner, DACL and integrity label as SDDL, for diagnostics.
pub fn handle_security_sddl(handle: &Handle) -> io::Result<String> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    };
    let which = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the out-pointers are locals; the descriptor is freed below.
    let status = unsafe {
        GetSecurityInfo(
            handle.raw(),
            SE_FILE_OBJECT,
            which,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = SecurityDescriptor(descriptor);
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `descriptor` is valid; the string is freed below.
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor.0,
            SDDL_REVISION_1,
            which,
            &mut text,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a NUL-terminated string allocated by the call above.
    let result = unsafe {
        let length = (0..).take_while(|&index| *text.add(index) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    };
    // SAFETY: allocated by ConvertSecurityDescriptorToStringSecurityDescriptorW.
    unsafe { LocalFree(text.cast()) };
    Ok(result)
}

/// A file's owner, DACL and integrity label as SDDL, for diagnostics.
pub fn path_security_sddl(path: &std::path::Path) -> io::Result<String> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::IntoRawHandle;
    // READ_CONTROL only: enough to read the descriptor, not the contents.
    const READ_CONTROL: u32 = 0x0002_0000;
    let file = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .open(path)?;
    let handle = Handle::from_raw(file.into_raw_handle())?;
    handle_security_sddl(&handle)
}

/// The current process user's SID in string form, such as `S-1-5-21-…`.
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle; the token handle is
    // written to a local and then owned.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle::from_raw(token)?;
    let mut needed = 0_u32;
    // SAFETY: a size query with no buffer.
    unsafe { GetTokenInformation(token.raw(), TokenUser, ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 elements keep the buffer aligned for TOKEN_USER.
    let mut buffer = vec![0_u64; (needed as usize).div_ceil(8)];
    // SAFETY: the buffer holds at least `needed` bytes.
    if unsafe {
        GetTokenInformation(
            token.raw(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so the buffer starts with a TOKEN_USER
    // whose SID points inside the same buffer.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` is valid while `buffer` lives; the string is freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ConvertSidToStringSidW returns a NUL-terminated string.
    let result = unsafe {
        let length = (0..).take_while(|&index| *text.add(index) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    };
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(text.cast()) };
    Ok(result)
}

/// Creates a file that must not exist yet, with the given security.
pub fn create_new_file(
    path: &std::path::Path,
    security: &SecurityDescriptor,
) -> io::Result<Handle> {
    use std::os::windows::ffi::OsStrExt;
    let name: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let attributes = security.attributes();
    // SAFETY: `name` is NUL-terminated; `attributes` and the descriptor it
    // points at outlive the call.
    Handle::from_raw(unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            &attributes,
            CREATE_NEW,
            0,
            ptr::null_mut(),
        )
    })
}

pub struct PipeInstance {
    pub handle: Handle,
}

/// Creates one server instance of a pipe. With `first`, the call fails when
/// any instance of that name already exists, so another process cannot have
/// claimed the name first.
pub fn create_pipe_instance(
    name: &[u16],
    security: &SecurityDescriptor,
    first: bool,
    buffer_bytes: u32,
) -> io::Result<PipeInstance> {
    let attributes = security.attributes();
    let mut open_mode: FILE_FLAGS_AND_ATTRIBUTES = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // SAFETY: `name` is NUL-terminated; `attributes` outlives the call.
    let handle = Handle::from_raw(unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            buffer_bytes,
            buffer_bytes,
            0,
            &attributes,
        )
    })?;
    Ok(PipeInstance { handle })
}

/// Why an overlapped wait ended without a result.
pub enum Wait {
    Done(u32),
    TimedOut,
}

/// One overlapped operation with its own event. Dropping it after a
/// timeout is safe because [`finish`] always waits for completion.
struct Pending {
    overlapped: Box<OVERLAPPED>,
    _event: Handle,
}

impl Pending {
    fn new() -> io::Result<Self> {
        // SAFETY: an unnamed manual-reset event with default security.
        let event = Handle::from_raw(unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) })?;
        // SAFETY: OVERLAPPED is plain data; zero is its documented initial state.
        let mut overlapped: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        overlapped.hEvent = event.raw();
        Ok(Self {
            overlapped,
            _event: event,
        })
    }

    fn as_ptr(&mut self) -> *mut OVERLAPPED {
        &mut *self.overlapped
    }
}

fn timeout_ms(timeout: Option<Duration>) -> u32 {
    timeout.map_or(INFINITE, |value| {
        u32::try_from(value.as_millis())
            .unwrap_or(INFINITE - 1)
            .min(INFINITE - 1)
    })
}

/// Waits for a started operation. On timeout it cancels and then waits for
/// the cancellation, so the kernel has finished with the buffer and the
/// OVERLAPPED before this returns.
fn finish(handle: &Handle, pending: &mut Pending, timeout: Option<Duration>) -> io::Result<Wait> {
    let mut transferred = 0_u32;
    // SAFETY: `pending` stays alive and pinned in its Box until this returns.
    if unsafe {
        GetOverlappedResultEx(
            handle.raw(),
            pending.as_ptr(),
            &mut transferred,
            timeout_ms(timeout),
            0,
        )
    } != 0
    {
        return Ok(Wait::Done(transferred));
    }
    // SAFETY: reading the calling thread's last error.
    let error = unsafe { GetLastError() };
    // A zero timeout reports ERROR_IO_INCOMPLETE rather than WAIT_TIMEOUT.
    if error == WAIT_TIMEOUT || error == ERROR_IO_INCOMPLETE {
        // SAFETY: cancel this operation only, then wait for it to settle.
        let settled = unsafe {
            CancelIoEx(handle.raw(), pending.as_ptr());
            GetOverlappedResult(handle.raw(), pending.as_ptr(), &mut transferred, 1)
        };
        // The operation may have completed just before the cancellation
        // reached it; its bytes are then real and must not be lost.
        if settled != 0 && transferred > 0 {
            return Ok(Wait::Done(transferred));
        }
        return Ok(Wait::TimedOut);
    }
    Err(io::Error::from_raw_os_error(error as i32))
}

fn started(result: i32) -> io::Result<()> {
    if result != 0 {
        return Ok(());
    }
    // SAFETY: reading the calling thread's last error.
    match unsafe { GetLastError() } {
        ERROR_IO_PENDING => Ok(()),
        error => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

fn is_closed(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error().map(|code| code as u32),
        Some(
            ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED | ERROR_OPERATION_ABORTED
        )
    )
}

/// Reads up to `buffer.len()` bytes. `Ok(Wait::Done(0))` means the peer
/// closed the pipe.
pub fn read(handle: &Handle, buffer: &mut [u8], timeout: Option<Duration>) -> io::Result<Wait> {
    let mut pending = Pending::new()?;
    let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    // SAFETY: the buffer and OVERLAPPED outlive the operation (see `finish`).
    let result = unsafe {
        ReadFile(
            handle.raw(),
            buffer.as_mut_ptr(),
            length,
            ptr::null_mut(),
            pending.as_ptr(),
        )
    };
    match started(result).and_then(|()| finish(handle, &mut pending, timeout)) {
        Err(error) if is_closed(&error) => Ok(Wait::Done(0)),
        other => other,
    }
}

/// Writes all of `buffer`, or fails. A timeout leaves the stream unusable.
pub fn write_all(handle: &Handle, mut buffer: &[u8], timeout: Option<Duration>) -> io::Result<()> {
    while !buffer.is_empty() {
        let mut pending = Pending::new()?;
        let length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: the buffer and OVERLAPPED outlive the operation.
        let result = unsafe {
            WriteFile(
                handle.raw(),
                buffer.as_ptr(),
                length,
                ptr::null_mut(),
                pending.as_ptr(),
            )
        };
        match started(result).and_then(|()| finish(handle, &mut pending, timeout)) {
            Ok(Wait::Done(0)) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(Wait::Done(written)) => buffer = &buffer[written as usize..],
            Ok(Wait::TimedOut) => return Err(io::ErrorKind::TimedOut.into()),
            Err(error) if is_closed(&error) => return Err(io::ErrorKind::BrokenPipe.into()),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Waits for a client on a server instance. `Ok(false)` on timeout.
pub fn connect(instance: &PipeInstance, timeout: Option<Duration>) -> io::Result<bool> {
    let mut pending = Pending::new()?;
    // SAFETY: the OVERLAPPED outlives the operation.
    let result = unsafe { ConnectNamedPipe(instance.handle.raw(), pending.as_ptr()) };
    if result == 0 {
        // SAFETY: reading the calling thread's last error.
        match unsafe { GetLastError() } {
            // A client connected between creation and this call.
            ERROR_PIPE_CONNECTED => return Ok(true),
            ERROR_IO_PENDING => {}
            error => return Err(io::Error::from_raw_os_error(error as i32)),
        }
    }
    match finish(&instance.handle, &mut pending, timeout)? {
        Wait::Done(_) => Ok(true),
        Wait::TimedOut => Ok(false),
    }
}

/// Flushes what the client has not read yet and disconnects it.
pub fn disconnect(instance: &PipeInstance) {
    // SAFETY: both calls only act on the owned handle.
    unsafe {
        FlushFileBuffers(instance.handle.raw());
        DisconnectNamedPipe(instance.handle.raw());
    }
}

pub enum OpenError {
    /// No engine is listening.
    NotFound,
    /// Every instance stayed busy for the whole wait.
    Busy,
    Other(io::Error),
}

/// Opens the client end, waiting up to `timeout` for a free instance. The
/// server may only identify, not impersonate, this client.
pub fn open_client(name: &[u16], timeout: Duration) -> Result<Handle, OpenError> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        // SAFETY: `name` is NUL-terminated.
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        if raw != INVALID_HANDLE_VALUE {
            return Handle::from_raw(raw).map_err(OpenError::Other);
        }
        // SAFETY: reading the calling thread's last error.
        match unsafe { GetLastError() } {
            ERROR_FILE_NOT_FOUND => return Err(OpenError::NotFound),
            ERROR_PIPE_BUSY => {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return Err(OpenError::Busy);
                }
                // SAFETY: `name` is NUL-terminated.
                let waited =
                    unsafe { WaitNamedPipeW(name.as_ptr(), timeout_ms(Some(left)).max(1)) };
                // SAFETY: reading the calling thread's last error.
                if waited == 0 && unsafe { GetLastError() } == ERROR_SEM_TIMEOUT {
                    return Err(OpenError::Busy);
                }
            }
            ERROR_ACCESS_DENIED => {
                return Err(OpenError::Other(io::Error::from_raw_os_error(
                    ERROR_ACCESS_DENIED as i32,
                )));
            }
            error => return Err(OpenError::Other(io::Error::from_raw_os_error(error as i32))),
        }
    }
}
