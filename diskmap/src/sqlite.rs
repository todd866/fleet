//! Thin sqlite3 C API. Links the system library (`bundled` is not used).

use std::cell::UnsafeCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const OPEN_READONLY: c_int = 0x0000_0001;
const OPEN_READWRITE: c_int = 0x0000_0002;
const OPEN_CREATE: c_int = 0x0000_0004;
const OPEN_FULLMUTEX: c_int = 0x0001_0000;

#[repr(C)]
struct Sqlite3 {
    _private: [u8; 0],
}
#[repr(C)]
struct Sqlite3Stmt {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        db: *mut *mut Sqlite3,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_busy_handler(
        db: *mut Sqlite3,
        cb: Option<unsafe extern "C" fn(*mut c_void, c_int) -> c_int>,
        arg: *mut c_void,
    ) -> c_int;
    fn sqlite3_close(db: *mut Sqlite3) -> c_int;
    fn sqlite3_exec(
        db: *mut Sqlite3,
        sql: *const c_char,
        cb: *mut c_void,
        arg: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut Sqlite3,
        sql: *const c_char,
        n: c_int,
        stmt: *mut *mut Sqlite3Stmt,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_bind_text(
        stmt: *mut Sqlite3Stmt,
        i: c_int,
        p: *const c_char,
        n: c_int,
        dtor: isize,
    ) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut Sqlite3Stmt, i: c_int, v: i64) -> c_int;
    fn sqlite3_step(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_column_text(stmt: *mut Sqlite3Stmt, i: c_int) -> *const u8;
    fn sqlite3_column_int64(stmt: *mut Sqlite3Stmt, i: c_int) -> i64;
    fn sqlite3_column_type(stmt: *mut Sqlite3Stmt, i: c_int) -> c_int;
    fn sqlite3_finalize(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_reset(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_clear_bindings(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_changes(db: *mut Sqlite3) -> c_int;
    fn sqlite3_errmsg(db: *mut Sqlite3) -> *const c_char;
    fn sqlite3_free(p: *mut c_void);
}

pub struct Db {
    raw: *mut Sqlite3,
    /// Start of the current statement's busy wait. The handler stops at 5s.
    busy_start: Box<UnsafeCell<Instant>>,
}

const BUSY_LIMIT: Duration = Duration::from_secs(5);

unsafe extern "C" fn busy_handler(arg: *mut c_void, _count: c_int) -> c_int {
    let start = unsafe { &*(arg as *const Instant) };
    if start.elapsed() >= BUSY_LIMIT {
        return 0;
    }
    std::thread::sleep(Duration::from_millis(25));
    if start.elapsed() >= BUSY_LIMIT {
        return 0;
    }
    1
}

pub struct Stmt {
    raw: *mut Sqlite3Stmt,
    db: *mut Sqlite3,
}

unsafe impl Send for Db {}

impl Db {
    pub fn open(path: &Path) -> Result<Self, String> {
        Self::open_with(path, OPEN_READWRITE | OPEN_CREATE | OPEN_FULLMUTEX, true)
    }

    pub fn open_readonly(path: &Path) -> Result<Self, String> {
        Self::open_with(path, OPEN_READONLY | OPEN_FULLMUTEX, false)
    }

    fn open_with(path: &Path, flags: c_int, writable: bool) -> Result<Self, String> {
        let c = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        let mut raw = ptr::null_mut();
        let rc = unsafe { sqlite3_open_v2(c.as_ptr(), &mut raw, flags, ptr::null()) };
        if rc != SQLITE_OK {
            let msg = errmsg(raw);
            if !raw.is_null() {
                unsafe { sqlite3_close(raw) };
            }
            return Err(format!("sqlite open: {msg}"));
        }
        let busy_start = Box::new(UnsafeCell::new(Instant::now()));
        let arg = busy_start.get() as *mut c_void;
        let busy = unsafe { sqlite3_busy_handler(raw, Some(busy_handler), arg) };
        if busy != SQLITE_OK {
            unsafe { sqlite3_close(raw) };
            return Err(format!("sqlite: {}", errmsg(raw)));
        }
        let db = Db { raw, busy_start };
        // Timeout before WAL. journal_mode takes a write lock, and a connection
        // that has not waited yet fails at once with "database is locked".
        if writable {
            db.exec("PRAGMA journal_mode=WAL;")?;
            db.exec("PRAGMA synchronous=NORMAL;")?;
        } else {
            db.exec("PRAGMA query_only=ON;")?;
        }
        Ok(db)
    }

    fn arm_busy(&self) {
        unsafe { *self.busy_start.get() = Instant::now() };
    }

    pub fn exec(&self, sql: &str) -> Result<(), String> {
        self.arm_busy();
        let c = CString::new(sql).map_err(|e| e.to_string())?;
        let mut err = ptr::null_mut();
        let rc = unsafe { sqlite3_exec(self.raw, c.as_ptr(), ptr::null_mut(), ptr::null_mut(), &mut err) };
        if rc != SQLITE_OK {
            let msg = if err.is_null() {
                errmsg(self.raw)
            } else {
                let s = unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned();
                unsafe { sqlite3_free(err.cast()) };
                s
            };
            return Err(format!("sqlite: {msg}"));
        }
        Ok(())
    }

    pub fn prepare(&self, sql: &str) -> Result<Stmt, String> {
        self.arm_busy();
        let c = CString::new(sql).map_err(|e| e.to_string())?;
        let mut stmt = ptr::null_mut();
        let rc = unsafe {
            sqlite3_prepare_v2(self.raw, c.as_ptr(), -1, &mut stmt, ptr::null_mut())
        };
        if rc != SQLITE_OK {
            return Err(format!("sqlite prepare: {}", errmsg(self.raw)));
        }
        Ok(Stmt { raw: stmt, db: self.raw })
    }

    pub fn changes(&self) -> i64 {
        unsafe { sqlite3_changes(self.raw) as i64 }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { sqlite3_close(self.raw) };
            self.raw = ptr::null_mut();
        }
    }
}

pub fn is_locked(err: &str) -> bool {
    let msg = err.to_ascii_lowercase();
    msg.contains("database is locked") || msg.contains("database table is locked")
}

fn errmsg(db: *mut Sqlite3) -> String {
    if db.is_null() {
        return "sqlite".into();
    }
    unsafe { CStr::from_ptr(sqlite3_errmsg(db)) }
        .to_string_lossy()
        .into_owned()
}

impl Stmt {
    pub fn bind_text(&self, i: i32, value: &str) -> Result<(), String> {
        let c = CString::new(value).map_err(|e| e.to_string())?;
        let rc = unsafe { sqlite3_bind_text(self.raw, i, c.as_ptr(), -1, -1) };
        if rc != SQLITE_OK {
            return Err(format!("sqlite bind: {}", errmsg(self.db)));
        }
        Ok(())
    }

    pub fn bind_i64(&self, i: i32, value: i64) -> Result<(), String> {
        let rc = unsafe { sqlite3_bind_int64(self.raw, i, value) };
        if rc != SQLITE_OK {
            return Err(format!("sqlite bind: {}", errmsg(self.db)));
        }
        Ok(())
    }

    pub fn step(&self) -> Result<bool, String> {
        let rc = unsafe { sqlite3_step(self.raw) };
        match rc {
            SQLITE_ROW => Ok(true),
            SQLITE_DONE => Ok(false),
            _ => Err(format!("sqlite step: {}", errmsg(self.db))),
        }
    }

    pub fn text(&self, i: i32) -> String {
        let ptr = unsafe { sqlite3_column_text(self.raw, i) };
        if ptr.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(ptr.cast()) }
            .to_string_lossy()
            .into_owned()
    }

    pub fn i64(&self, i: i32) -> i64 {
        unsafe { sqlite3_column_int64(self.raw, i) }
    }

    pub fn is_null(&self, i: i32) -> bool {
        unsafe { sqlite3_column_type(self.raw, i) == 5 }
    }

    pub fn reset(&self) -> Result<(), String> {
        unsafe {
            sqlite3_reset(self.raw);
            sqlite3_clear_bindings(self.raw);
        }
        Ok(())
    }
}

impl Drop for Stmt {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { sqlite3_finalize(self.raw) };
            self.raw = ptr::null_mut();
        }
    }
}
