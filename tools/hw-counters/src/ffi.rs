//! The Windows functions and structures the service uses (kernel32, advapi32), declared by hand like the rest of ree.
//! The layouts are the 64-bit ones, checked at compile time against the sizes and offsets the SDK headers give.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;

pub type Handle = *mut c_void;
pub const INVALID_HANDLE: Handle = -1isize as Handle;
pub const INFINITE: u32 = 0xFFFF_FFFF;
pub const WAIT_OBJECT_0: u32 = 0;

pub const ERROR_ACCESS_DENIED: u32 = 5;
pub const ERROR_INVALID_FUNCTION: u32 = 1;
pub const ERROR_NOT_SUPPORTED: u32 = 50;
pub const ERROR_INVALID_PARAMETER: u32 = 87;
pub const ERROR_BUSY: u32 = 170;
pub const ERROR_ALREADY_EXISTS: u32 = 183;
pub const ERROR_PIPE_CONNECTED: u32 = 535;
pub const ERROR_IO_PENDING: u32 = 997;
pub const ERROR_PRIVILEGE_NOT_HELD: u32 = 1314;
pub const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
pub const ERROR_FAILED_SERVICE_CONTROLLER_CONNECT: u32 = 1063;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

/// The kernel's thread events (Thread_V2..V4): Start 1, End 2, DCStart 3, DCEnd 4, CSwitch 36
pub const THREAD_GUID: Guid = Guid { data1: 0x3d6f_a8d1, data2: 0xfe05, data3: 0x11d0, data4: [0x9d, 0xda, 0x00, 0xc0, 0x4f, 0xd7, 0xba, 0x7c] };
pub const SYSTEM_TRACE_CONTROL_GUID: Guid = Guid { data1: 0x9e81_4aad, data2: 0x3204, data3: 0x11d2, data4: [0x9a, 0x82, 0x00, 0x60, 0x08, 0xa8, 0x69, 0x39] };
/// The service's own session's GUID (a system logger must not use SystemTraceControlGuid)
pub const SESSION_GUID: Guid = Guid { data1: 0x5d3c_9e41, data2: 0x6b1a, data3: 0x4c2e, data4: [0x9f, 0x0d, 0x51, 0x7e, 0x2a, 0x83, 0x64, 0xc7] };

#[repr(C)]
pub struct SecurityAttributes {
    pub length: u32,
    pub descriptor: *mut c_void,
    pub inherit: i32,
}

#[repr(C)]
pub struct Overlapped {
    pub internal: usize,
    pub internal_high: usize,
    pub offset: u32,
    pub offset_high: u32,
    pub event: Handle,
}

#[repr(C)]
pub struct ThreadEntry32 {
    pub size: u32,
    pub usage: u32,
    pub thread_id: u32,
    pub owner_process_id: u32,
    pub base_priority: i32,
    pub delta_priority: i32,
    pub flags: u32,
}

#[repr(C)]
pub struct ServiceTableEntry {
    pub name: *mut u16,
    pub main: Option<unsafe extern "system" fn(u32, *mut *mut u16)>,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ServiceStatus {
    pub service_type: u32,
    pub current_state: u32,
    pub controls_accepted: u32,
    pub win32_exit_code: u32,
    pub service_specific_exit_code: u32,
    pub check_point: u32,
    pub wait_hint: u32,
}

#[repr(C)]
pub struct StartupInfo {
    pub cb: u32,
    pub reserved: *mut u16,
    pub desktop: *mut u16,
    pub title: *mut u16,
    pub x: u32,
    pub y: u32,
    pub x_size: u32,
    pub y_size: u32,
    pub x_count_chars: u32,
    pub y_count_chars: u32,
    pub fill_attribute: u32,
    pub flags: u32,
    pub show_window: u16,
    pub reserved2_size: u16,
    pub reserved2: *mut u8,
    pub std_input: Handle,
    pub std_output: Handle,
    pub std_error: Handle,
}

#[repr(C)]
pub struct ProcessInformation {
    pub process: Handle,
    pub thread: Handle,
    pub process_id: u32,
    pub thread_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Luid {
    pub low: u32,
    pub high: i32,
}

/// TOKEN_PRIVILEGES with one privilege
#[repr(C)]
pub struct TokenPrivileges {
    pub count: u32,
    pub luid: Luid,
    pub attributes: u32,
}

/// Enables a privilege the process's token holds (a service's listed privileges may start disabled): false when the
/// token does not hold it
pub fn enable_privilege(name: &str) -> bool {
    const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
    const TOKEN_QUERY: u32 = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;
    const ERROR_NOT_ALL_ASSIGNED: u32 = 1300;
    unsafe {
        let mut token: Handle = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let token = Owned(token);
        let mut luid = Luid::default();
        let wide_name = wide(name);
        if LookupPrivilegeValueW(std::ptr::null(), wide_name.as_ptr(), &mut luid) == 0 {
            return false;
        }
        let privileges = TokenPrivileges { count: 1, luid, attributes: SE_PRIVILEGE_ENABLED };
        AdjustTokenPrivileges(token.0, 0, &privileges, 0, std::ptr::null_mut(), std::ptr::null_mut()) != 0 && GetLastError() != ERROR_NOT_ALL_ASSIGNED
    }
}

// --- ETW (evntrace.h, evntcons.h) ---

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WnodeHeader {
    pub buffer_size: u32,
    pub provider_id: u32,
    pub historical_context: u64,
    pub timestamp: i64,
    pub guid: Guid,
    pub client_context: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct EventTraceProperties {
    pub wnode: WnodeHeader,
    pub buffer_size: u32,
    pub minimum_buffers: u32,
    pub maximum_buffers: u32,
    pub maximum_file_size: u32,
    pub log_file_mode: u32,
    pub flush_timer: u32,
    pub enable_flags: u32,
    pub age_limit: i32,
    pub number_of_buffers: u32,
    pub free_buffers: u32,
    pub events_lost: u32,
    pub buffers_written: u32,
    pub log_buffers_lost: u32,
    pub real_time_buffers_lost: u32,
    pub logger_thread_id: usize,
    pub log_file_name_offset: u32,
    pub logger_name_offset: u32,
}

/// EVENT_TRACE_PROPERTIES with room for the session's name after it, where StartTrace and ControlTrace put it
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Properties {
    pub properties: EventTraceProperties,
    pub name: [u16; 1024],
}

#[repr(C)]
pub struct EventTraceLogfile {
    pub log_file_name: *mut u16,
    pub logger_name: *mut u16,
    pub current_time: i64,
    pub buffers_read: u32,
    pub process_trace_mode: u32,
    /// EVENT_TRACE (88 bytes) and TRACE_LOGFILE_HEADER (280): filled by ETW, read by nobody here
    pub current_event: [u8; 88],
    pub logfile_header: [u8; 280],
    pub buffer_callback: usize,
    pub buffer_size: u32,
    pub filled: u32,
    pub events_lost: u32,
    pub event_record_callback: Option<unsafe extern "system" fn(*mut EventRecord)>,
    pub is_kernel_trace: u32,
    pub context: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct EventDescriptor {
    pub id: u16,
    pub version: u8,
    pub channel: u8,
    pub level: u8,
    pub opcode: u8,
    pub task: u16,
    pub keyword: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct EventHeader {
    pub size: u16,
    pub header_type: u16,
    pub flags: u16,
    pub event_property: u16,
    pub thread_id: u32,
    pub process_id: u32,
    pub timestamp: i64,
    pub provider_id: Guid,
    pub descriptor: EventDescriptor,
    pub processor_time: u64,
    pub activity_id: Guid,
}

#[repr(C)]
pub struct EventRecord {
    pub header: EventHeader,
    /// ETW_BUFFER_CONTEXT: ProcessorNumber (u8) and Alignment, or ProcessorIndex (u16); then LoggerId
    pub processor: u16,
    pub logger_id: u16,
    pub extended_data_count: u16,
    pub user_data_length: u16,
    pub extended_data: *const ExtendedItem,
    pub user_data: *const c_void,
    pub user_context: *mut c_void,
}

#[repr(C)]
pub struct ExtendedItem {
    pub reserved1: u16,
    pub ext_type: u16,
    pub linkage: u16,
    pub data_size: u16,
    pub data_ptr: u64,
}

/// CLASSIC_EVENT_ID: an event of the kernel's, by its class GUID and type (opcode)
#[repr(C)]
pub struct ClassicEventId {
    pub guid: Guid,
    pub event_type: u8,
    pub reserved: [u8; 7],
}

pub const WNODE_FLAG_TRACED_GUID: u32 = 0x0002_0000;
pub const EVENT_TRACE_REAL_TIME_MODE: u32 = 0x0000_0100;
pub const EVENT_TRACE_SYSTEM_LOGGER_MODE: u32 = 0x0200_0000;
pub const EVENT_TRACE_FLAG_THREAD: u32 = 0x0000_0002;
pub const EVENT_TRACE_FLAG_CSWITCH: u32 = 0x0000_0010;
pub const EVENT_TRACE_CONTROL_QUERY: u32 = 0;
pub const EVENT_TRACE_CONTROL_STOP: u32 = 1;
pub const EVENT_TRACE_CONTROL_FLUSH: u32 = 3;
pub const PROCESS_TRACE_MODE_REAL_TIME: u32 = 0x0000_0100;
pub const PROCESS_TRACE_MODE_RAW_TIMESTAMP: u32 = 0x0000_1000;
pub const PROCESS_TRACE_MODE_EVENT_RECORD: u32 = 0x1000_0000;
pub const INVALID_PROCESSTRACE_HANDLE: u64 = u64::MAX;
pub const EVENT_HEADER_FLAG_PROCESSOR_INDEX: u16 = 0x0200;
pub const EVENT_HEADER_EXT_TYPE_PMC_COUNTERS: u16 = 0x0008;
/// TRACE_INFO_CLASS
pub const TRACE_PROFILE_SOURCE_LIST_INFO: u32 = 7;
pub const TRACE_PMC_EVENT_LIST_INFO: u32 = 8;
pub const TRACE_PMC_COUNTER_LIST_INFO: u32 = 9;
pub const TRACE_MAX_PMC_COUNTER_QUERY: u32 = 22;

const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<WnodeHeader>() == 48);
    assert!(size_of::<EventTraceProperties>() == 120);
    assert!(offset_of!(EventTraceProperties, events_lost) == 88);
    assert!(offset_of!(EventTraceProperties, real_time_buffers_lost) == 100);
    assert!(offset_of!(EventTraceProperties, logger_thread_id) == 104);
    assert!(offset_of!(EventTraceProperties, logger_name_offset) == 116);
    assert!(offset_of!(Properties, name) == 120);
    assert!(size_of::<EventTraceLogfile>() == 448);
    assert!(offset_of!(EventTraceLogfile, logfile_header) == 120);
    assert!(offset_of!(EventTraceLogfile, buffer_callback) == 400);
    assert!(offset_of!(EventTraceLogfile, event_record_callback) == 424);
    assert!(offset_of!(EventTraceLogfile, context) == 440);
    assert!(size_of::<EventDescriptor>() == 16);
    assert!(size_of::<EventHeader>() == 80);
    assert!(offset_of!(EventHeader, timestamp) == 16);
    assert!(offset_of!(EventHeader, provider_id) == 24);
    assert!(offset_of!(EventHeader, descriptor) == 40);
    assert!(size_of::<EventRecord>() == 112);
    assert!(offset_of!(EventRecord, processor) == 80);
    assert!(offset_of!(EventRecord, extended_data_count) == 84);
    assert!(offset_of!(EventRecord, user_data_length) == 86);
    assert!(offset_of!(EventRecord, extended_data) == 88);
    assert!(offset_of!(EventRecord, user_data) == 96);
    assert!(offset_of!(EventRecord, user_context) == 104);
    assert!(size_of::<ExtendedItem>() == 16);
    assert!(size_of::<ClassicEventId>() == 24);
    assert!(size_of::<Overlapped>() == 32);
    assert!(size_of::<ThreadEntry32>() == 28);
    assert!(size_of::<StartupInfo>() == 104);
    assert!(size_of::<ProcessInformation>() == 24);
    assert!(size_of::<TokenPrivileges>() == 16);
};

unsafe extern "system" {
    pub fn CloseHandle(handle: Handle) -> i32;
    pub fn GetLastError() -> u32;
    pub fn LocalFree(memory: *mut c_void) -> *mut c_void;
    pub fn CreateEventW(attributes: *mut c_void, manual_reset: i32, initial: i32, name: *const u16) -> Handle;
    pub fn SetEvent(event: Handle) -> i32;
    pub fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    pub fn WaitForMultipleObjects(count: u32, handles: *const Handle, wait_all: i32, milliseconds: u32) -> u32;
    pub fn CreateNamedPipeW(name: *const u16, open_mode: u32, pipe_mode: u32, instances: u32, out_size: u32, in_size: u32, timeout: u32, security: *mut SecurityAttributes) -> Handle;
    pub fn ConnectNamedPipe(pipe: Handle, overlapped: *mut Overlapped) -> i32;
    pub fn DisconnectNamedPipe(pipe: Handle) -> i32;
    pub fn GetNamedPipeClientProcessId(pipe: Handle, process: *mut u32) -> i32;
    // (the overlapped structure as a plain pointer: client.rs declares them so too)
    pub fn ReadFile(file: Handle, buffer: *mut u8, to_read: u32, read: *mut u32, overlapped: *mut c_void) -> i32;
    pub fn WriteFile(file: Handle, buffer: *const u8, to_write: u32, written: *mut u32, overlapped: *mut c_void) -> i32;
    pub fn GetOverlappedResult(file: Handle, overlapped: *mut Overlapped, transferred: *mut u32, wait: i32) -> i32;
    pub fn CancelIoEx(file: Handle, overlapped: *mut Overlapped) -> i32;
    pub fn FlushFileBuffers(file: Handle) -> i32;
    pub fn OpenProcess(access: u32, inherit: i32, process: u32) -> Handle;
    pub fn OpenThread(access: u32, inherit: i32, thread: u32) -> Handle;
    pub fn GetProcessIdOfThread(thread: Handle) -> u32;
    pub fn GetCurrentProcess() -> Handle;
    pub fn GetCurrentThread() -> Handle;
    pub fn GetCurrentProcessorNumber() -> u32;
    pub fn SetThreadAffinityMask(thread: Handle, mask: usize) -> usize;
    pub fn CreateToolhelp32Snapshot(flags: u32, process: u32) -> Handle;
    pub fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry32) -> i32;
    pub fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry32) -> i32;
    pub fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
    pub fn GetActiveProcessorCount(group: u16) -> u32;
    pub fn GetModuleFileNameW(module: Handle, name: *mut u16, size: u32) -> u32;
    pub fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    pub fn CreateProcessW(application: *const u16, command_line: *mut u16, process_attributes: *mut c_void, thread_attributes: *mut c_void, inherit: i32, flags: u32, environment: *mut c_void, directory: *const u16, startup: *mut StartupInfo, information: *mut ProcessInformation) -> i32;
    pub fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
    pub fn TerminateProcess(process: Handle, code: u32) -> i32;
    pub fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    pub fn StartTraceW(handle: *mut u64, name: *const u16, properties: *mut EventTraceProperties) -> u32;
    pub fn ControlTraceW(handle: u64, name: *const u16, properties: *mut EventTraceProperties, code: u32) -> u32;
    pub fn TraceSetInformation(handle: u64, class: u32, information: *const c_void, length: u32) -> u32;
    pub fn TraceQueryInformation(handle: u64, class: u32, information: *mut c_void, length: u32, returned: *mut u32) -> u32;
    pub fn OpenTraceW(logfile: *mut EventTraceLogfile) -> u64;
    pub fn ProcessTrace(handles: *const u64, count: u32, start: *mut c_void, end: *mut c_void) -> u32;
    pub fn CloseTrace(handle: u64) -> u32;
    pub fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    pub fn GetTokenInformation(token: Handle, class: u32, information: *mut c_void, length: u32, returned: *mut u32) -> i32;
    pub fn EqualSid(a: *const c_void, b: *const c_void) -> i32;
    pub fn GetLengthSid(sid: *const c_void) -> u32;
    pub fn IsValidSid(sid: *const c_void) -> i32;
    pub fn ConvertSidToStringSidW(sid: *const c_void, text: *mut *mut u16) -> i32;
    pub fn ConvertStringSidToSidW(text: *const u16, sid: *mut *mut c_void) -> i32;
    pub fn ConvertStringSecurityDescriptorToSecurityDescriptorW(text: *const u16, revision: u32, descriptor: *mut *mut c_void, size: *mut u32) -> i32;
    pub fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut Luid) -> i32;
    pub fn AdjustTokenPrivileges(token: Handle, disable_all: i32, new: *const TokenPrivileges, length: u32, previous: *mut c_void, returned: *mut u32) -> i32;
    pub fn StartServiceCtrlDispatcherW(table: *const ServiceTableEntry) -> i32;
    pub fn RegisterServiceCtrlHandlerExW(name: *const u16, handler: Option<unsafe extern "system" fn(u32, u32, *mut c_void, *mut c_void) -> u32>, context: *mut c_void) -> Handle;
    pub fn SetServiceStatus(handle: Handle, status: *mut ServiceStatus) -> i32;
}

/// A provider of events of its own (the tests check the consumer's layouts on a session any member of Performance Log
/// Users may run)
#[cfg(test)]
pub mod provider {
    use super::{EventDescriptor, Guid};
    use std::ffi::c_void;

    #[repr(C)]
    pub struct EnableTraceParameters {
        pub version: u32,
        pub enable_property: u32,
        pub control_flags: u32,
        pub source_id: Guid,
        pub enable_filter_desc: *mut c_void,
        pub filter_desc_count: u32,
    }

    #[repr(C)]
    pub struct EventDataDescriptor {
        pub ptr: u64,
        pub size: u32,
        pub reserved: u32,
    }

    pub const ENABLE_TRACE_PARAMETERS_VERSION_2: u32 = 2;
    pub const EVENT_ENABLE_PROPERTY_SID: u32 = 0x1;
    pub const EVENT_CONTROL_CODE_ENABLE_PROVIDER: u32 = 1;

    const _: () = assert!(std::mem::size_of::<EnableTraceParameters>() == 48);

    #[link(name = "advapi32")]
    unsafe extern "system" {
        pub fn EventRegister(provider: *const Guid, callback: *mut c_void, context: *mut c_void, handle: *mut u64) -> u32;
        pub fn EventUnregister(handle: u64) -> u32;
        pub fn EventWrite(handle: u64, descriptor: *const EventDescriptor, count: u32, data: *const EventDataDescriptor) -> u32;
        pub fn EnableTraceEx2(session: u64, provider: *const Guid, control: u32, level: u8, any: u64, all: u64, timeout: u32, parameters: *const EnableTraceParameters) -> u32;
    }
}

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A NUL-terminated UTF-16 string
///
/// # Safety
/// `text` is null or points at a NUL-terminated string.
pub unsafe fn from_wide(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut length = 0;
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) })
}

/// A handle closed when dropped
pub struct Owned(pub Handle);

// (a kernel handle is the process's, not a thread's: a client's pipe and thread handles move to the thread serving it)
unsafe impl Send for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// A security identifier's bytes
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sid(pub Vec<u8>);

impl Sid {
    /// From its text (`S-1-5-21-...`)
    pub fn parse(text: &str) -> Result<Sid, String> {
        let wide = wide(text);
        let mut sid: *mut c_void = std::ptr::null_mut();
        unsafe {
            if ConvertStringSidToSidW(wide.as_ptr(), &mut sid) == 0 {
                return Err(format!("{text:?} is not a security identifier"));
            }
            let length = GetLengthSid(sid) as usize;
            let bytes = std::slice::from_raw_parts(sid as *const u8, length).to_vec();
            LocalFree(sid);
            Ok(Sid(bytes))
        }
    }

    /// The user a process runs as
    pub fn of_process(process: Handle) -> Result<Sid, String> {
        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_USER: u32 = 1;
        unsafe {
            let mut token: Handle = std::ptr::null_mut();
            if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
                return Err(format!("the process's token: error {}", GetLastError()));
            }
            let token = Owned(token);
            let mut buffer = vec![0u8; 256];
            let mut returned = 0u32;
            if GetTokenInformation(token.0, TOKEN_USER, buffer.as_mut_ptr() as *mut c_void, buffer.len() as u32, &mut returned) == 0 {
                return Err(format!("the token's user: error {}", GetLastError()));
            }
            // TOKEN_USER: SID_AND_ATTRIBUTES { PSID Sid; DWORD Attributes }
            let sid = *(buffer.as_ptr() as *const *const c_void);
            if IsValidSid(sid) == 0 {
                return Err("the token's user is not a valid security identifier".into());
            }
            Ok(Sid(std::slice::from_raw_parts(sid as *const u8, GetLengthSid(sid) as usize).to_vec()))
        }
    }

    pub fn text(&self) -> String {
        unsafe {
            let mut text: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(self.0.as_ptr() as *const c_void, &mut text) == 0 {
                return "?".into();
            }
            let result = from_wide(text);
            LocalFree(text as *mut c_void);
            result
        }
    }

    pub fn equals(&self, other: &Sid) -> bool {
        unsafe { EqualSid(self.0.as_ptr() as *const c_void, other.0.as_ptr() as *const c_void) != 0 }
    }
}
