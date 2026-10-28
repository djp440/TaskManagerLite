//! 私有内核 ETW 会话：只订阅 `DiskIo` 与 `Thread` 事件，按进程累计磁盘字节数（见 ADR-0004）。
//!
//! - 会话名带本进程 PID、创建时间与序号，不占用全局唯一的 “NT Kernel Logger”（不与 Process Monitor 等冲突）。
//! - 会话在后台线程里启动与消费，调用方不会被阻塞；需要管理员权限，没有时不尝试。
//! - 只有调用 [`DiskTrace::start`] 才会碰 ETW：不启动时不清理残留、不装钩子、没有任何开销。
//! - 正常退出（[`TraceGuard`]）、panic（panic 钩子）与未处理异常（异常过滤器）都会停止会话；
//!   进程被强行结束时无法清理，下一次启动会话时会按会话名里的 PID + 创建时间识别并停止残留会话。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};

use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, FILETIME};
use windows::Win32::System::Diagnostics::Debug::{EXCEPTION_POINTERS, SetUnhandledExceptionFilter};
use windows::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, CloseTrace, ControlTraceW, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_FLAG_DISK_IO,
    EVENT_TRACE_FLAG_THREAD, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE,
    EVENT_TRACE_SYSTEM_LOGGER_MODE, OpenTraceW, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME,
    ProcessTrace, QueryAllTracesW, StartTraceW, WNODE_FLAG_TRACED_GUID,
};
use windows::Win32::System::SystemInformation::GetSystemInfo;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, GetProcessIdOfThread, GetProcessTimes, OpenThread, PROCESS_QUERY_LIMITED_INFORMATION,
    THREAD_QUERY_LIMITED_INFORMATION,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::control::{create_time, is_elevated, open};
use crate::diskio::{DISK_IO_GUID, DiskAggregator, Event, THREAD_GUID, Window, parse};
use crate::system::filetime;

/// 会话名前缀。完整名称：`TaskManagerLite-DiskIo-<PID>-<创建时间十六进制>-<序号>`。
const SESSION_PREFIX: &str = "TaskManagerLite-DiskIo-";

/// 缓冲区大小（KB）。磁盘事件每条约 100 字节，每秒几百条，16 KB 足够；缓冲区是非分页内存，越小越省。
const BUFFER_KB: u32 = 16;
/// 查询残留会话时先容纳的会话数（系统默认上限 64）；返回 `ERROR_MORE_DATA` 时扩大到 [`MAX_SESSIONS_RETRY`] 再查一次。
const MAX_SESSIONS: usize = 80;
const MAX_SESSIONS_RETRY: usize = 256;
const STILL_ACTIVE: u32 = 259;

/// 同一进程里先后启动的会话序号（运行中开关设置时会重复启动），保证名称不重复。
static SEQ: AtomicU32 = AtomicU32::new(0);

pub fn session_name(pid: u32, create_time: u64, seq: u32) -> String {
    format!("{SESSION_PREFIX}{pid}-{create_time:x}-{seq}")
}

/// 从会话名取出属主进程的 PID 与创建时间；不是本程序的会话名则返回 `None`。
pub fn parse_session_name(name: &str) -> Option<(u32, u64)> {
    let mut parts = name.strip_prefix(SESSION_PREFIX)?.split('-');
    let pid = parts.next()?.parse().ok()?;
    let ct = u64::from_str_radix(parts.next()?, 16).ok()?;
    parts.next()?.parse::<u32>().ok()?;
    parts.next().is_none().then_some((pid, ct))
}

/// 属主进程是否已经不存在。`probe` 是对该 PID 的探测结果：
/// `Err(code)` 为打开失败的 Win32 错误码，`Ok((创建时间, 是否仍在运行))` 为打开成功后读到的信息（读不到为 `None`）。
/// 只有明确“PID 不存在”（`ERROR_INVALID_PARAMETER`）、创建时间对不上（PID 已被复用）或确认已退出才算不存在；
/// 拒绝访问等其他错误一律视为存活，宁可漏清也不误停别人的会话。
fn owner_gone(probe: Result<(Option<u64>, Option<bool>), u32>, create_time: u64) -> bool {
    match probe {
        Err(code) => code == ERROR_INVALID_PARAMETER.0,
        Ok((Some(ct), _)) if ct != create_time => true,
        Ok((_, Some(false))) => true,
        Ok(_) => false,
    }
}

fn probe_owner(pid: u32) -> Result<(Option<u64>, Option<bool>), u32> {
    let h = open(pid, PROCESS_QUERY_LIMITED_INFORMATION).map_err(|e| (e.code().0 as u32) & 0xffff)?;
    let mut code = 0u32;
    let active = unsafe { GetExitCodeProcess(h.raw(), &mut code) }.ok().map(|_| code == STILL_ACTIVE);
    Ok((create_time(&h), active))
}

/// 会话的运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceState {
    /// 后台线程正在启动会话，还没有数据。
    Starting,
    Running,
    /// 启动失败（Win32 错误码），或会话中途被外部停止。
    Failed(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    /// 内核会话需要管理员权限；未提权时直接放弃，不做任何尝试。
    NotElevated,
}

const STATE_STARTING: u8 = 0;
const STATE_RUNNING: u8 = 1;
const STATE_FAILED: u8 = 2;

struct Shared {
    agg: Mutex<DiskAggregator>,
    state: AtomicU8,
    error: AtomicU32,
    /// `StartTrace` 已成功：只有这时本对象才有会话需要停止。
    started: AtomicBool,
    /// 调用方已丢弃 `DiskTrace`：启动线程看到后立即停止会话。
    cancel: AtomicBool,
    ctx: AtomicPtr<StopCtx>,
}

impl Shared {
    fn agg(&self) -> std::sync::MutexGuard<'_, DiskAggregator> {
        self.agg.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fail(&self, code: u32) {
        self.error.store(code, Ordering::Release);
        self.state.store(STATE_FAILED, Ordering::Release);
    }

    fn stop(&self) {
        let p = self.ctx.load(Ordering::Acquire);
        if !p.is_null() {
            // SAFETY: StopCtx 泄漏为 'static，永不释放。
            unsafe { &*p }.stop();
        }
    }
}

/// 一个正在运行（或启动中）的磁盘事件会话。
pub struct DiskTrace {
    shared: Arc<Shared>,
}

impl DiskTrace {
    /// 在后台线程启动会话并立即返回。未提权时返回 `Err`，此时不碰 ETW。
    pub fn start() -> Result<Self, StartError> {
        if !is_elevated() {
            return Err(StartError::NotElevated);
        }
        Ok(Self::start_unchecked())
    }

    /// 不检查权限（测试用：未提权时会话应以 `Failed` 告终，而不是崩溃）。
    fn start_unchecked() -> Self {
        install_crash_hooks();
        let name = session_name(std::process::id(), own_create_time(), SEQ.fetch_add(1, Ordering::Relaxed));
        let shared = Arc::new(Shared {
            agg: Mutex::default(),
            state: AtomicU8::new(STATE_STARTING),
            error: AtomicU32::new(0),
            started: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            ctx: AtomicPtr::new(std::ptr::null_mut()),
        });
        let s = shared.clone();
        if std::thread::Builder::new().name("etw-disk".into()).spawn(move || run_session(&s, &name)).is_err() {
            shared.fail(u32::MAX);
        }
        Self { shared }
    }

    pub fn state(&self) -> TraceState {
        match self.shared.state.load(Ordering::Acquire) {
            STATE_STARTING => TraceState::Starting,
            STATE_RUNNING => TraceState::Running,
            _ => TraceState::Failed(self.shared.error.load(Ordering::Acquire)),
        }
    }

    /// 取出自上次调用以来的累计结果（每进程读 + 写字节数、事件数与无法归属的事件数），并清零。
    pub fn take(&self) -> Window {
        self.shared.agg().take()
    }
}

impl Drop for DiskTrace {
    fn drop(&mut self) {
        self.shared.cancel.store(true, Ordering::SeqCst);
        // 只停止自己 StartTrace 成功的会话；启动线程尚未走到那一步时，它会看到 cancel 后自行停止。
        if self.shared.started.load(Ordering::SeqCst) {
            self.shared.stop();
        }
    }
}

/// 在 `main` 开头持有：函数以任何方式返回（含 `?`）时停止本进程的 ETW 会话。
pub struct TraceGuard;

impl Drop for TraceGuard {
    fn drop(&mut self) {
        stop_all();
    }
}

/// 停止本进程最近一次创建的会话（可重复调用，没有会话时什么也不做）。
/// 也在 panic 钩子与异常过滤器里调用：只用原子操作和一次 `ControlTraceW`，不分配内存、不加锁。
pub fn stop_all() {
    let p = LATEST.load(Ordering::Acquire);
    if !p.is_null() {
        // SAFETY: StopCtx 泄漏为 'static，永不释放。
        unsafe { &*p }.stop();
    }
}

/// 停止会话所需的一切，在会话启动成功后预先构造并泄漏（几 KB），使崩溃处理路径不必分配内存。
struct StopCtx {
    props: *mut EVENT_TRACE_PROPERTIES,
    /// 以 0 结尾的 UTF-16 会话名。
    name: Vec<u16>,
    armed: AtomicBool,
}

impl StopCtx {
    fn arm(name: &str) -> &'static StopCtx {
        let props: &'static mut Props = Box::leak(Box::new(Props::new(1024, 1024)));
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        Box::leak(Box::new(StopCtx { props: props.ptr(), name, armed: AtomicBool::new(true) }))
    }

    /// 只停止一次；之后的调用什么也不做（也避免两个线程同时使用同一块 properties 缓冲）。
    fn stop(&self) {
        if self.armed.swap(false, Ordering::AcqRel) {
            let _ = unsafe {
                ControlTraceW(CONTROLTRACE_HANDLE::default(), PCWSTR(self.name.as_ptr()), self.props, EVENT_TRACE_CONTROL_STOP)
            };
        }
    }
}

/// 本进程最近一次启动成功的会话。
static LATEST: AtomicPtr<StopCtx> = AtomicPtr::new(std::ptr::null_mut());
static PREVIOUS_FILTER: AtomicUsize = AtomicUsize::new(0);

fn install_crash_hooks() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // release 构建是 panic = abort：钩子在 abort 之前运行。
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            stop_all();
            previous(info);
        }));
        unsafe extern "system" fn on_exception(info: *const EXCEPTION_POINTERS) -> i32 {
            stop_all();
            let prev = PREVIOUS_FILTER.load(Ordering::Acquire);
            if prev == 0 {
                return 0; // EXCEPTION_CONTINUE_SEARCH
            }
            // SAFETY: 值来自 SetUnhandledExceptionFilter 返回的函数指针。
            let prev: unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32 = unsafe { std::mem::transmute(prev) };
            unsafe { prev(info) }
        }
        let old = unsafe { SetUnhandledExceptionFilter(Some(on_exception)) };
        PREVIOUS_FILTER.store(old.map_or(0, |f| f as usize), Ordering::Release);
    });
}

fn own_create_time() -> u64 {
    let mut t = [FILETIME::default(); 4];
    let [c, e, k, u] = &mut t;
    let _ = unsafe { GetProcessTimes(GetCurrentProcess(), c, e, k, u) };
    filetime(t[0])
}

/// `EVENT_TRACE_PROPERTIES` 加上其后的名称空间，按 8 字节对齐分配。
struct Props {
    buf: Vec<u64>,
    name_chars: usize,
}

impl Props {
    fn new(name_chars: usize, file_chars: usize) -> Self {
        let head = size_of::<EVENT_TRACE_PROPERTIES>();
        let total = head + 2 * (name_chars + file_chars);
        let mut p = Self { buf: vec![0; total.div_ceil(8)], name_chars };
        let r = p.get();
        r.Wnode.BufferSize = total as u32;
        r.LoggerNameOffset = head as u32;
        r.LogFileNameOffset = if file_chars > 0 { (head + 2 * name_chars) as u32 } else { 0 };
        p
    }

    fn get(&mut self) -> &mut EVENT_TRACE_PROPERTIES {
        // SAFETY: 缓冲区足够大且 8 字节对齐，全零是该结构的合法值。
        unsafe { &mut *self.buf.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>() }
    }

    fn ptr(&mut self) -> *mut EVENT_TRACE_PROPERTIES {
        self.buf.as_mut_ptr().cast()
    }

    fn set_name(&mut self, name: &str) {
        let off = self.get().LoggerNameOffset as usize;
        let chars = self.name_chars;
        // SAFETY: 名称区位于缓冲区内，长度由 `new` 保证。
        let dst = unsafe { std::slice::from_raw_parts_mut(self.buf.as_mut_ptr().cast::<u8>().add(off).cast::<u16>(), chars) };
        for (d, s) in dst.iter_mut().zip(name.encode_utf16().chain(Some(0))) {
            *d = s;
        }
    }

    fn name(&mut self) -> String {
        let off = self.get().LoggerNameOffset as usize;
        let chars = self.name_chars;
        // SAFETY: 同 `set_name`。
        let src = unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>().add(off).cast::<u16>(), chars) };
        String::from_utf16_lossy(src.split(|&c| c == 0).next().unwrap_or_default())
    }
}

/// 按名称停止会话；不存在时返回错误码，调用方可忽略。只用于清理残留会话（非崩溃路径，可以分配）。
fn stop_session(name: &str) -> u32 {
    let mut p = Props::new(1024, 1024);
    unsafe { ControlTraceW(CONTROLTRACE_HANDLE::default(), &HSTRING::from(name), p.ptr(), EVENT_TRACE_CONTROL_STOP) }.0
}

/// 枚举当前所有 ETW 会话的名称。容量不够（`ERROR_MORE_DATA`）时扩大后重试一次。
fn list_sessions() -> Vec<String> {
    for capacity in [MAX_SESSIONS, MAX_SESSIONS_RETRY] {
        let mut bufs: Vec<Props> = (0..capacity).map(|_| Props::new(1024, 1024)).collect();
        let mut ptrs: Vec<*mut EVENT_TRACE_PROPERTIES> = bufs.iter_mut().map(Props::ptr).collect();
        let mut count = 0u32;
        let err = unsafe { QueryAllTracesW(&mut ptrs, &mut count) };
        if err == ERROR_MORE_DATA && capacity < MAX_SESSIONS_RETRY {
            continue;
        }
        if err.0 != 0 && err != ERROR_MORE_DATA {
            return Vec::new();
        }
        return bufs.iter_mut().take((count as usize).min(capacity)).map(Props::name).collect();
    }
    Vec::new()
}

/// 停止属主进程已不存在的残留会话；返回停止的个数。
fn sweep_stale() -> usize {
    let mut stopped = 0;
    for name in list_sessions() {
        let Some((pid, ct)) = parse_session_name(&name) else { continue };
        if owner_gone(probe_owner(pid), ct) && stop_session(&name) == 0 {
            stopped += 1;
        }
    }
    stopped
}

/// 后台线程主体：清理残留 → 启动会话 → 阻塞消费事件，直到会话被停止。
fn run_session(shared: &Arc<Shared>, name: &str) {
    if shared.cancel.load(Ordering::SeqCst) {
        shared.fail(0);
        return;
    }
    sweep_stale();

    let cpus = unsafe {
        let mut si = std::mem::zeroed();
        GetSystemInfo(&mut si);
        si.dwNumberOfProcessors.max(1)
    };
    let mut p = Props::new(name.encode_utf16().count() + 1, 0);
    p.set_name(name);
    {
        let r = p.get();
        r.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        r.Wnode.ClientContext = 1; // 事件时间戳取 QPC
        r.BufferSize = BUFFER_KB;
        // 每个处理器至少两块缓冲区；上限留一倍余量，突发时不丢事件。
        r.MinimumBuffers = 2 * cpus;
        r.MaximumBuffers = 4 * cpus;
        r.FlushTimer = 1; // 实时模式下缓冲区最迟 1 秒送达消费者
        r.LogFileMode = EVENT_TRACE_REAL_TIME_MODE | EVENT_TRACE_SYSTEM_LOGGER_MODE;
        r.EnableFlags = EVENT_TRACE_FLAG_DISK_IO | EVENT_TRACE_FLAG_THREAD;
    }
    let mut handle = CONTROLTRACE_HANDLE::default();
    let err = unsafe { StartTraceW(&mut handle, &HSTRING::from(name), p.ptr()) }.0;
    if err != 0 {
        shared.fail(err);
        return;
    }
    // 会话已存在：先备好停止用的上下文，再公布 started，任何一条退出路径都能停掉它。
    let ctx = StopCtx::arm(name);
    shared.ctx.store(ctx as *const StopCtx as *mut StopCtx, Ordering::Release);
    LATEST.store(ctx as *const StopCtx as *mut StopCtx, Ordering::Release);
    shared.started.store(true, Ordering::SeqCst);
    if shared.cancel.load(Ordering::SeqCst) {
        ctx.stop();
        shared.fail(0);
        return;
    }

    let mut logger: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut log = EVENT_TRACE_LOGFILEW { LoggerName: PWSTR(logger.as_mut_ptr()), Context: Arc::as_ptr(shared) as *mut c_void, ..Default::default() };
    log.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    log.Anonymous2.EventRecordCallback = Some(on_event);
    let consumer = unsafe { OpenTraceW(&mut log) };
    // INVALID_PROCESSTRACE_HANDLE
    if consumer.Value == u64::MAX || consumer.Value == u64::from(u32::MAX) {
        shared.fail(unsafe { windows::Win32::Foundation::GetLastError() }.0);
        ctx.stop();
        return;
    }
    shared.state.store(STATE_RUNNING, Ordering::Release);
    // 阻塞到会话被停止（Drop、stop_all 或外部工具）。
    let _ = unsafe { ProcessTrace(&[consumer], None, None) };
    let _ = unsafe { CloseTrace(consumer) };
    // ProcessTrace 因为别的原因返回、会话还在时，保证停掉；已停止则什么也不做。
    ctx.stop();
    shared.fail(0);
}

/// 向系统查询线程所属进程（线程表里没有的线程，例如启动前的枚举事件丢失时）。
fn pid_of_thread(tid: u32) -> Option<u32> {
    let h = unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, false, tid) }.ok()?;
    let pid = unsafe { GetProcessIdOfThread(h) };
    let _ = unsafe { CloseHandle(h) };
    (pid != 0).then_some(pid)
}

/// ETW 事件回调，在 `ProcessTrace` 所在线程上调用。
unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    // SAFETY: 系统保证 record 在回调期间有效；UserContext 是 `run_session` 传入的 `Shared` 指针，
    // 其所属 Arc 在 ProcessTrace 返回前一直存活。
    let r = unsafe { &*record };
    let provider = r.EventHeader.ProviderId.to_u128();
    if provider != DISK_IO_GUID && provider != THREAD_GUID {
        return;
    }
    let shared = unsafe { &*(r.UserContext as *const Shared) };
    let payload: &[u8] = if r.UserData.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(r.UserData as *const u8, r.UserDataLength as usize) }
    };
    let h = &r.EventHeader;
    let Some(event) = parse(provider, h.EventDescriptor.Opcode, h.EventDescriptor.Version, h.Flags, h.ProcessId, payload) else {
        return;
    };
    // 常见路径只加一次锁；只有线程表里没有、本窗口也没查过的线程，才在锁外向系统查询后再加一次锁。
    let mut agg = shared.agg();
    if let Event::DiskIo { tid: Some(tid), .. } = event
        && !agg.knows(tid)
        && !agg.is_missing(tid)
    {
        drop(agg);
        let pid = pid_of_thread(tid);
        agg = shared.agg();
        match pid {
            Some(pid) => agg.learn(tid, pid),
            None => agg.mark_missing(tid),
        }
    }
    agg.apply(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_round_trips() {
        let n = session_name(1234, 0x01dc_ab12_3456_7890, 3);
        assert_eq!(n, "TaskManagerLite-DiskIo-1234-1dcab1234567890-3");
        assert_eq!(parse_session_name(&n), Some((1234, 0x01dc_ab12_3456_7890)));
    }

    #[test]
    fn foreign_session_names_are_ignored() {
        for n in [
            "NT Kernel Logger",
            "EventLog-System",
            "TaskManagerLite-DiskIo-",
            "TaskManagerLite-DiskIo-12",
            "TaskManagerLite-DiskIo-12-ab",
            "TaskManagerLite-DiskIo-x-1-0",
            "TaskManagerLite-DiskIo-1-zz-0",
            "TaskManagerLite-DiskIo-1-2-x",
            "TaskManagerLite-DiskIo-1-2-3-4",
            "Other-TaskManagerLite-DiskIo-1-2-3",
        ] {
            assert_eq!(parse_session_name(n), None, "{n}");
        }
    }

    #[test]
    fn only_a_definitely_dead_owner_counts_as_stale() {
        let ct = 0x1234;
        // PID 不存在：残留。
        assert!(owner_gone(Err(ERROR_INVALID_PARAMETER.0), ct));
        // 拒绝访问或其他错误：当作存活，不动别人的会话。
        assert!(!owner_gone(Err(5), ct));
        assert!(!owner_gone(Err(1), ct));
        // PID 被复用（创建时间对不上）、或确认已退出：残留。
        assert!(owner_gone(Ok((Some(ct + 1), Some(true))), ct));
        assert!(owner_gone(Ok((Some(ct), Some(false))), ct));
        // 同一个进程且仍在运行，或信息读不到：存活。
        assert!(!owner_gone(Ok((Some(ct), Some(true))), ct));
        assert!(!owner_gone(Ok((None, None)), ct));
        assert!(!owner_gone(Ok((Some(ct), None)), ct));
    }

    #[test]
    fn current_process_is_not_stale() {
        assert!(!owner_gone(probe_owner(std::process::id()), own_create_time()));
        // 这个创建时间不属于当前进程：认为 PID 已被复用。
        assert!(owner_gone(probe_owner(std::process::id()), own_create_time() + 1));
    }

    #[test]
    fn props_hold_the_logger_name() {
        let mut p = Props::new(64, 0);
        p.set_name("TaskManagerLite-DiskIo-1-2-3");
        assert_eq!(p.name(), "TaskManagerLite-DiskIo-1-2-3");
        let head = size_of::<EVENT_TRACE_PROPERTIES>();
        assert_eq!(p.get().Wnode.BufferSize as usize, head + 128);
        assert_eq!(p.get().LoggerNameOffset as usize, head);
        assert_eq!(p.get().LogFileNameOffset, 0);
    }

    #[test]
    fn stop_context_is_prebuilt_and_stops_only_once() {
        let ctx = StopCtx::arm("TaskManagerLite-DiskIo-1-2-3");
        assert_eq!(ctx.name.last(), Some(&0));
        assert!(ctx.armed.load(Ordering::SeqCst));
        // 会话并不存在：调用只是返回错误，但“已停止”标记要翻转，第二次不再调用系统。
        ctx.stop();
        assert!(!ctx.armed.load(Ordering::SeqCst));
        ctx.stop();
    }

    #[test]
    fn start_without_elevation_is_refused() {
        if !is_elevated() {
            assert_eq!(DiskTrace::start().err(), Some(StartError::NotElevated));
        }
    }

    /// 未提权时强行启动：后台线程应以 Failed 告终（拒绝访问），不挂起、不崩溃、不遗留会话。
    #[test]
    fn forced_start_without_elevation_fails_cleanly() {
        if is_elevated() {
            return; // 提权环境会真的创建会话，交给验收脚本
        }
        let t = DiskTrace::start_unchecked();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while t.state() == TraceState::Starting && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let TraceState::Failed(code) = t.state() else { panic!("未提权却启动成功: {:?}", t.state()) };
        println!("未提权启动 ETW 会话的错误码: {code}");
        assert_ne!(code, 0);
        assert_eq!(t.take(), Window::default());
        assert!(!t.shared.started.load(Ordering::SeqCst));
    }
}
