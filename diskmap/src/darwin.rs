//! macOS volume listing, getattrlistbulk sizes, and FSEvents replay.
//! The mapper never writes user files and does not materialize dataless files
//! when `be_gentle` is on (the launchd tick).

use crate::logic::{RawEvent, HISTORY_DONE, ITEM_IS_DIR, ITEM_IS_FILE, MUST_SCAN_SUBDIRS};
use std::ffi::{CStr, CString};
use std::fs;
use std::os::raw::{c_char, c_int, c_void};
use std::os::unix::fs::MetadataExt;
#[cfg(target_os = "macos")]
use std::os::darwin::fs::MetadataExt as DarwinMeta;
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{Duration, Instant};

const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_VOL_UUID: u32 = 0x0004_0000;
const ATTR_DIR_MOUNTSTATUS: u32 = 0x0000_0004;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;
const FSOPT_PACK_INVAL_ATTRS: u64 = 0x8;
const DIR_MNTSTATUS_MNTPOINT: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;
const MNT_LOCAL: u32 = 0x0000_1000;
const MNT_SNAPSHOT: u32 = 0x4000_0000;
const MNT_NOWAIT: c_int = 2;
const QOS_CLASS_BACKGROUND: u32 = 0x09;
const IOPOL_TYPE_DISK: i32 = 0;
const IOPOL_SCOPE_PROCESS: i32 = 0;
const IOPOL_THROTTLE: i32 = 3;
const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: i32 = 3;
const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: i32 = 1;
const FILE_EVENTS: u32 = 0x10;
const WATCH_ROOT: u32 = 0x4;
const UTF8: u32 = 0x0800_0100;

#[repr(C)]
struct Attrlist {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

#[repr(C)]
struct FsEventStreamContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

unsafe extern "C" {
    fn getattrlist(
        path: *const c_char,
        attrlist: *mut Attrlist,
        attrbuf: *mut c_void,
        attrbufsize: usize,
        options: u64,
    ) -> c_int;
    fn getattrlistbulk(
        dirfd: c_int,
        attrlist: *mut Attrlist,
        attrbuf: *mut c_void,
        attrbufsize: usize,
        options: u64,
    ) -> c_int;
    fn uuid_unparse_upper(uu: *const u8, out: *mut c_char);
    fn pthread_set_qos_class_self_np(class: u32, rel: i32) -> i32;
    fn setiopolicy_np(typ: i32, scope: i32, policy: i32) -> i32;
    fn FSEventStreamCreateRelativeToDevice(
        allocator: *const c_void,
        callback: unsafe extern "C" fn(
            *const c_void,
            *mut c_void,
            usize,
            *mut c_void,
            *const u32,
            *const u64,
        ),
        context: *mut FsEventStreamContext,
        device: i32,
        paths: *const c_void,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> *mut c_void;
    fn FSEventStreamScheduleWithRunLoop(stream: *mut c_void, rl: *const c_void, mode: *const c_void);
    fn FSEventStreamStart(stream: *mut c_void) -> u8;
    fn FSEventStreamStop(stream: *mut c_void);
    fn FSEventStreamInvalidate(stream: *mut c_void);
    fn FSEventStreamRelease(stream: *mut c_void);
    fn FSEventsCopyUUIDForDevice(dev: i32) -> *const c_void;
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        c: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFArrayCreate(
        alloc: *const c_void,
        values: *const *const c_void,
        num: isize,
        callbacks: *const c_void,
    ) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFUUIDCreateString(alloc: *const c_void, uuid: *const c_void) -> *const c_void;
    fn CFStringGetCString(s: *const c_void, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRunLoopGetCurrent() -> *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after: u8) -> i32;
    static kCFRunLoopDefaultMode: *const c_void;
    static kCFTypeArrayCallBacks: u8;
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub symlink: bool,
    pub mount_point: bool,
    pub allocated: u64,
    pub logical: u64,
    pub flags: u32,
    pub mtime: i64,
}

#[derive(Clone, Debug)]
pub struct Mounted {
    pub mount: PathBuf,
    pub uuid: String,
    pub name: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub dev: i32,
}

pub fn be_gentle() {
    unsafe {
        pthread_set_qos_class_self_np(QOS_CLASS_BACKGROUND, 0);
        setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS, IOPOL_THROTTLE);
        setiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_PROCESS,
            IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
        );
    }
}

/// The walk worker's own priority, set by the worker itself so it holds whoever
/// launches it (launchd, the tick, a shell): nice 20 and the Darwin background
/// band (CPU on efficiency cores, throttled disk and network). Returns the
/// (nice, darwin_bg) values read back.
pub fn worker_priority() -> (i32, bool) {
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 20);
        libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, libc::PRIO_DARWIN_BG);
    }
    read_priority()
}

pub fn read_priority() -> (i32, bool) {
    unsafe {
        *libc::__error() = 0;
        let nice = libc::getpriority(libc::PRIO_PROCESS, 0);
        let bg = libc::getpriority(libc::PRIO_DARWIN_PROCESS, 0);
        (nice, bg > 0)
    }
}

/// CPU time used by this process and its reaped children (the focus-guard
/// checks), user plus system.
pub fn cpu_time() -> Duration {
    fn of(who: c_int) -> Duration {
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(who, &mut ru) } != 0 {
            return Duration::ZERO;
        }
        let tv = |t: libc::timeval| Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64);
        tv(ru.ru_utime) + tv(ru.ru_stime)
    }
    of(libc::RUSAGE_SELF) + of(libc::RUSAGE_CHILDREN)
}

/// The directory being read right now and since when, for the walk worker's
/// watchdog: a launchd-started worker blocked in open(2) on one directory for
/// good (18 Nov 2026) and nothing else could move it.
static READING: std::sync::Mutex<Option<(Instant, PathBuf)>> = std::sync::Mutex::new(None);
/// Directories that blocked a worker before; never opened again.
static SKIP: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

pub fn reading_now() -> Option<(Duration, PathBuf)> {
    let guard = READING.lock().ok()?;
    guard.as_ref().map(|(t, p)| (t.elapsed(), p.clone()))
}

pub fn set_skip(paths: Vec<PathBuf>) {
    if let Ok(mut guard) = SKIP.lock() {
        *guard = paths;
    }
}

pub fn skipped(path: &Path) -> bool {
    SKIP.lock().map(|g| g.iter().any(|p| p == path)).unwrap_or(false)
}

pub fn read_dir_entries(path: &Path) -> Result<Vec<Entry>, String> {
    if skipped(path) {
        return Err(format!("{}: skipped, it blocked a worker before", path.display()));
    }
    if let Ok(mut guard) = READING.lock() {
        *guard = Some((Instant::now(), path.to_path_buf()));
    }
    let out = read_dir_unwatched(path);
    if let Ok(mut guard) = READING.lock() {
        *guard = None;
    }
    out
}

fn read_dir_unwatched(path: &Path) -> Result<Vec<Entry>, String> {
    match read_dir_bulk(path) {
        Ok(entries) => Ok(entries),
        Err(err) => read_dir_std(path).map_err(|fallback| format!("{err}; {fallback}")),
    }
}

fn read_dir_bulk(path: &Path) -> Result<Vec<Entry>, String> {
    let c = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY) };
    if fd < 0 {
        return Err(format!("open {}: {}", path.display(), std::io::Error::last_os_error()));
    }
    let mut attr = Attrlist {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_NAME
            | ATTR_CMN_ERROR
            | ATTR_CMN_OBJTYPE
            | ATTR_CMN_FLAGS
            | ATTR_CMN_MODTIME,
        volattr: 0,
        dirattr: ATTR_DIR_MOUNTSTATUS,
        fileattr: ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH,
        forkattr: 0,
    };
    let mut buf = vec![0u8; 256 * 1024];
    let mut out = Vec::new();
    loop {
        let n = unsafe {
            getattrlistbulk(
                fd,
                &mut attr,
                buf.as_mut_ptr().cast(),
                buf.len(),
                FSOPT_PACK_INVAL_ATTRS,
            )
        };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(format!("getattrlistbulk {}: {err}", path.display()));
        }
        if n == 0 {
            break;
        }
        let parsed = parse_bulk(&buf, n);
        if parsed.is_empty() {
            unsafe { libc::close(fd) };
            return Err(format!("getattrlistbulk {}: unreadable record", path.display()));
        }
        out.extend(parsed);
    }
    unsafe { libc::close(fd) };
    Ok(out)
}

fn parse_bulk(buf: &[u8], count: i32) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut base = 0usize;
    for _ in 0..count {
        if base + 4 > buf.len() {
            break;
        }
        let len = ru32(buf, base) as usize;
        if len < 4 || base + len > buf.len() {
            break;
        }
        let rec = &buf[base..base + len];
        if let Some(entry) = parse_record(rec) {
            if entry.name != "." && entry.name != ".." {
                entries.push(entry);
            }
        }
        base += len;
    }
    entries
}

fn parse_record(rec: &[u8]) -> Option<Entry> {
    // Length, then the returned attribute set (always first). ATTR_CMN_ERROR,
    // when requested, is the next field — ahead of the other common bits.
    // After that, common bits run low to high, then directory, then file.
    // Directory records omit file sizes and file records omit mount status;
    // the returned groups say which fields are present.
    if rec.len() < 24 {
        return None;
    }
    let common = ru32(rec, 4);
    let dirattr = ru32(rec, 12);
    let fileattr = ru32(rec, 16);
    let mut cur = 24usize;
    let mut error = 0u32;
    let mut name = String::new();
    let mut obj_type = 0u32;
    let mut mtime = 0i64;
    let mut flags = 0u32;
    let mut mount = 0u32;
    let mut allocated = 0u64;
    let mut logical = 0u64;
    if common & ATTR_CMN_ERROR != 0 {
        error = take_u32(rec, &mut cur)?;
    }
    if common & ATTR_CMN_NAME != 0 {
        let rel = take_i32(rec, &mut cur)?;
        let nlen = take_u32(rec, &mut cur)? as usize;
        let at = (cur as i64 - 8 + rel as i64).max(0) as usize;
        if at < rec.len() {
            let end = (at + nlen).min(rec.len());
            name = String::from_utf8_lossy(&rec[at..end])
                .trim_end_matches('\0')
                .to_string();
        }
    }
    if common & ATTR_CMN_OBJTYPE != 0 {
        obj_type = take_u32(rec, &mut cur)?;
    }
    if common & ATTR_CMN_MODTIME != 0 {
        mtime = take_i64(rec, &mut cur)?;
        cur = cur.saturating_add(8);
    }
    if common & ATTR_CMN_FLAGS != 0 {
        flags = take_u32(rec, &mut cur)?;
    }
    if dirattr & ATTR_DIR_MOUNTSTATUS != 0 {
        mount = take_u32(rec, &mut cur)?;
    }
    if fileattr & ATTR_FILE_ALLOCSIZE != 0 {
        allocated = take_i64(rec, &mut cur)?.max(0) as u64;
    }
    if fileattr & ATTR_FILE_DATALENGTH != 0 {
        logical = take_i64(rec, &mut cur)?.max(0) as u64;
    }
    if name.is_empty() || error != 0 {
        return None;
    }
    let symlink = obj_type == VLNK;
    let is_dir = obj_type == VDIR && !symlink;
    Some(Entry {
        name,
        is_dir,
        symlink,
        mount_point: mount & DIR_MNTSTATUS_MNTPOINT != 0,
        allocated,
        logical,
        flags,
        mtime,
    })
}

fn take_u32(buf: &[u8], cur: &mut usize) -> Option<u32> {
    if *cur + 4 > buf.len() {
        return None;
    }
    let v = ru32(buf, *cur);
    *cur += 4;
    Some(v)
}
fn take_i32(buf: &[u8], cur: &mut usize) -> Option<i32> {
    if *cur + 4 > buf.len() {
        return None;
    }
    let v = ri32(buf, *cur);
    *cur += 4;
    Some(v)
}
fn take_i64(buf: &[u8], cur: &mut usize) -> Option<i64> {
    if *cur + 8 > buf.len() {
        return None;
    }
    let v = ri64(buf, *cur);
    *cur += 8;
    Some(v)
}

fn ru32(buf: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap())
}
fn ri32(buf: &[u8], off: usize) -> i32 {
    i32::from_ne_bytes(buf[off..off + 4].try_into().unwrap())
}
fn ri64(buf: &[u8], off: usize) -> i64 {
    i64::from_ne_bytes(buf[off..off + 8].try_into().unwrap())
}

fn read_dir_std(path: &Path) -> Result<Vec<Entry>, String> {
    let parent_dev = fs::metadata(path).map(|m| m.dev()).unwrap_or(0);
    let mut out = Vec::new();
    let rd = fs::read_dir(path).map_err(|e| e.to_string())?;
    for ent in rd {
        let ent = ent.map_err(|e| e.to_string())?;
        let name = ent.file_name().to_string_lossy().into_owned();
        let meta = match ent.path().symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let symlink = meta.file_type().is_symlink();
        let is_dir = meta.is_dir() && !symlink;
        let mount_point = is_dir && meta.dev() != parent_dev && parent_dev != 0;
        out.push(Entry {
            name,
            is_dir,
            symlink,
            mount_point,
            allocated: meta.blocks().saturating_mul(512),
            logical: meta.len(),
            flags: st_flags(&meta),
            mtime: meta.mtime(),
        });
    }
    Ok(out)
}

fn st_flags(meta: &fs::Metadata) -> u32 {
    #[cfg(target_os = "macos")]
    {
        DarwinMeta::st_flags(meta)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = meta;
        0
    }
}

pub fn list_volumes() -> Result<Vec<Mounted>, String> {
    let mut buf = vec![unsafe { std::mem::zeroed::<libc::statfs>() }; 256];
    let bytes = (buf.len() * std::mem::size_of::<libc::statfs>()) as c_int;
    let n = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, MNT_NOWAIT) };
    if n < 0 {
        return Err(format!("getfsstat: {}", std::io::Error::last_os_error()));
    }
    let mut raw = Vec::new();
    for st in buf.into_iter().take(n as usize) {
        let mount = c_chars(&st.f_mntonname);
        let fstype = c_chars(&st.f_fstypename);
        if !interesting(&mount, &fstype, st.f_flags) {
            continue;
        }
        let total = st.f_blocks.saturating_mul(st.f_bsize as u64);
        let free = st.f_bavail.saturating_mul(st.f_bsize as u64);
        let dev = fsid_dev(&st);
        let uuid = volume_uuid(Path::new(&mount)).unwrap_or_default();
        let name = mount.rsplit('/').next().unwrap_or(&mount).to_string();
        let name = if mount == "/" || mount == "/System/Volumes/Data" {
            "internal".to_string()
        } else {
            name
        };
        raw.push(Mounted {
            mount: PathBuf::from(mount),
            uuid,
            name,
            total_bytes: total,
            free_bytes: free,
            dev,
        });
    }
    let data_dev = raw
        .iter()
        .find(|v| v.mount == Path::new("/System/Volumes/Data"))
        .map(|v| v.dev);
    if data_dev.is_some() {
        raw.retain(|v| v.mount != Path::new("/") || Some(v.dev) == data_dev);
        if raw.iter().filter(|v| v.mount == Path::new("/") || v.mount == Path::new("/System/Volumes/Data")).count() > 1 {
            raw.retain(|v| v.mount != Path::new("/"));
        }
    }
    Ok(raw)
}

fn interesting(mount: &str, fstype: &str, flags: u32) -> bool {
    if flags & MNT_SNAPSHOT != 0 {
        return false;
    }
    if flags & MNT_LOCAL == 0 && mount.starts_with("/Volumes/") {
        return false;
    }
    let fs_ok = matches!(
        fstype,
        "apfs" | "hfs" | "exfat" | "msdos" | "ntfs" | "ufsd_NTFS" | "udf"
    );
    if !fs_ok {
        return false;
    }
    if mount == "/System/Volumes/Data" || mount == "/" {
        return true;
    }
    if mount.starts_with("/System/") || mount.starts_with("/dev") || mount.starts_with("/private/var/vm") {
        return false;
    }
    mount.starts_with("/Volumes/")
}

fn fsid_dev(st: &libc::statfs) -> i32 {
    // f_fsid.val[0] is the dev_t FSEvents wants.
    unsafe {
        let ptr = &st.f_fsid as *const libc::fsid_t as *const i32;
        ptr.read_unaligned()
    }
}

fn c_chars(bytes: &[c_char]) -> String {
    unsafe { CStr::from_ptr(bytes.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

pub fn volume_uuid(mount: &Path) -> Option<String> {
    let c = CString::new(mount.to_string_lossy().as_bytes()).ok()?;
    let mut attr = Attrlist {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buf = [0u8; 64];
    let rc = unsafe {
        getattrlist(
            c.as_ptr(),
            &mut attr,
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    if buf.len() < 4 + 16 {
        return None;
    }
    let mut text = [0u8; 37];
    unsafe {
        uuid_unparse_upper(buf.as_ptr().add(4), text.as_mut_ptr().cast());
    }
    let s = CStr::from_bytes_until_nul(&text).ok()?.to_string_lossy().into_owned();
    if s.len() < 32 { None } else { Some(s) }
}

struct Replay {
    events: Vec<RawEvent>,
    max_id: u64,
    history_done: bool,
}

unsafe extern "C" fn fs_callback(
    _stream: *const c_void,
    info: *mut c_void,
    num: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    if info.is_null() {
        return;
    }
    let replay = unsafe { &mut *(info as *mut Replay) };
    let path_ptrs = paths as *const *const c_char;
    for i in 0..num {
        let flag = unsafe { *flags.add(i) };
        let id = unsafe { *ids.add(i) };
        if id > replay.max_id && id != u64::MAX {
            replay.max_id = id;
        }
        if flag & HISTORY_DONE != 0 {
            replay.history_done = true;
            continue;
        }
        let p = unsafe { CStr::from_ptr(*path_ptrs.add(i)) }
            .to_string_lossy()
            .into_owned();
        replay.events.push(RawEvent {
            path: p,
            flags: flag,
            id,
        });
    }
}

/// Replay FSEvents since `since_id` (pass `u64::MAX` for "since now").
/// Returns events, the latest id to store, and the FSEvents device UUID.
pub fn replay_events(mount: &Path, dev: i32, since_id: u64, wait: Duration) -> Result<(Vec<RawEvent>, u64, String), String> {
    let uuid = device_uuid(dev);
    let empty = CString::new("").map_err(|e| e.to_string())?;
    let cf_str = unsafe { CFStringCreateWithCString(ptr::null(), empty.as_ptr(), UTF8) };
    if cf_str.is_null() {
        return Err("CFString".into());
    }
    let values = [cf_str];
    let array = unsafe {
        CFArrayCreate(
            ptr::null(),
            values.as_ptr(),
            1,
            &kCFTypeArrayCallBacks as *const u8 as *const c_void,
        )
    };
    if array.is_null() {
        unsafe { CFRelease(cf_str) };
        return Err("CFArray".into());
    }
    let mut replay = Replay {
        events: Vec::new(),
        max_id: if since_id == u64::MAX { 0 } else { since_id },
        history_done: since_id == u64::MAX,
    };
    let mut ctx = FsEventStreamContext {
        version: 0,
        info: &mut replay as *mut Replay as *mut c_void,
        retain: ptr::null(),
        release: ptr::null(),
        copy_description: ptr::null(),
    };
    let stream = unsafe {
        FSEventStreamCreateRelativeToDevice(
            ptr::null(),
            fs_callback,
            &mut ctx,
            dev,
            array,
            since_id,
            0.3,
            FILE_EVENTS | WATCH_ROOT,
        )
    };
    unsafe {
        CFRelease(array);
        CFRelease(cf_str);
    }
    if stream.is_null() {
        return Err(format!("FSEvents stream failed for {}", mount.display()));
    }
    unsafe {
        let mode = kCFRunLoopDefaultMode;
        FSEventStreamScheduleWithRunLoop(stream, CFRunLoopGetCurrent(), mode);
        if FSEventStreamStart(stream) == 0 {
            FSEventStreamInvalidate(stream);
            FSEventStreamRelease(stream);
            return Err("FSEventStreamStart".into());
        }
        let deadline = Instant::now() + wait;
        while !replay.history_done && Instant::now() < deadline {
            CFRunLoopRunInMode(mode, 0.2, 0);
        }
        FSEventStreamStop(stream);
        FSEventStreamInvalidate(stream);
        FSEventStreamRelease(stream);
    }
    // Relative-to-device paths are already volume-relative. Drop the mount
    // prefix if a stream ever returns an absolute path.
    let prefix = mount.to_string_lossy().trim_end_matches('/').to_string();
    for event in &mut replay.events {
        if event.path.starts_with(&prefix) {
            event.path = event.path[prefix.len()..].trim_start_matches('/').to_string();
        }
        let _ = (ITEM_IS_FILE, ITEM_IS_DIR, MUST_SCAN_SUBDIRS);
    }
    Ok((replay.events, replay.max_id, uuid))
}

fn device_uuid(dev: i32) -> String {
    unsafe {
        let uuid = FSEventsCopyUUIDForDevice(dev);
        if uuid.is_null() {
            return String::new();
        }
        let s = CFUUIDCreateString(ptr::null(), uuid);
        CFRelease(uuid);
        if s.is_null() {
            return String::new();
        }
        let mut buf = [0u8; 64];
        let ok = CFStringGetCString(s, buf.as_mut_ptr().cast(), buf.len() as isize, UTF8);
        CFRelease(s);
        if ok == 0 {
            return String::new();
        }
        CStr::from_ptr(buf.as_ptr().cast())
            .to_string_lossy()
            .into_owned()
    }
}

pub fn notify(title: &str, body: &str) {
    if std::env::var("DISKMAP_NO_NOTIFY").ok().as_deref() == Some("1") {
        return;
    }
    let script = format!("display notification \"{}\" with title \"{}\"", escape_applescript(body), escape_applescript(title));
    let _ = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn escape_applescript(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn focus_active() -> bool {
    std::process::Command::new("focus-guard")
        .arg("active")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Same thermal rule as focus-guard: a machine-log line under 3 minutes old
/// whose `thermal` is not nominal, or whose chip is above 60°C.
/// Serious or critical thermal pressure on a fresh machine-log line.
/// Fair, nominal, and a hot chip alone do not stop the long walk.
pub fn thermal_blocks_walk(line: &str, now: i64) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    let Some(t) = value.get("t").and_then(|v| v.as_i64()) else {
        return false;
    };
    if now.saturating_sub(t) > 180 {
        return false;
    }
    value
        .get("thermal")
        .and_then(|v| v.as_str())
        .is_some_and(|s| {
            let s = s.to_ascii_lowercase();
            s == "serious" || s == "critical"
        })
}

pub fn local_host_name() -> String {
    if let Ok(name) = std::env::var("DISKMAP_HOST") {
        if !name.is_empty() {
            return name;
        }
    }
    let out = std::process::Command::new("/usr/sbin/scutil")
        .arg("--get")
        .arg("LocalHostName")
        .output();
    if let Ok(out) = out {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    gethostname_fallback()
}

fn gethostname_fallback() -> String {
    let mut buf = [0u8; 256];
    unsafe {
        libc::gethostname(buf.as_mut_ptr().cast(), buf.len());
    }
    CStr::from_bytes_until_nul(&buf)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "localhost".into())
}
