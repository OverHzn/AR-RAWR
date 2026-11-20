//! Minimal safe wrapper around the system libsqlite3 (3.45.1) via raw FFI.
//! No external crate: `#[link(name = "sqlite3")]` is enough for offline builds.
//!
//! FTS5 (trigram tokenizer, contentless tables, optimize/merge) is available in
//! the linked library and is exercised by the traffic index slice.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;

use crate::db::error::{Error, Result};

#[allow(non_camel_case_types)]
pub enum sqlite3 {}
#[allow(non_camel_case_types)]
pub enum sqlite3_stmt {}

#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        db: *mut *mut sqlite3,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close(db: *mut sqlite3) -> c_int;
    fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;
    fn sqlite3_exec(
        db: *mut sqlite3,
        sql: *const c_char,
        cb: Option<
            unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int,
        >,
        arg: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut sqlite3,
        sql: *const c_char,
        nbyte: c_int,
        stmt: *mut *mut sqlite3_stmt,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_finalize(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_bind_null(stmt: *mut sqlite3_stmt, idx: c_int) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut sqlite3_stmt, idx: c_int, v: i64) -> c_int;
    fn sqlite3_bind_text(
        stmt: *mut sqlite3_stmt,
        idx: c_int,
        v: *const c_char,
        n: c_int,
        dtor: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int;
    fn sqlite3_bind_blob(
        stmt: *mut sqlite3_stmt,
        idx: c_int,
        v: *const c_void,
        n: c_int,
        dtor: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int;
    fn sqlite3_column_type(stmt: *mut sqlite3_stmt, col: c_int) -> c_int;
    fn sqlite3_column_int64(stmt: *mut sqlite3_stmt, col: c_int) -> i64;
    fn sqlite3_column_text(stmt: *mut sqlite3_stmt, col: c_int) -> *const u8;
    fn sqlite3_column_blob(stmt: *mut sqlite3_stmt, col: c_int) -> *const c_void;
    fn sqlite3_column_bytes(stmt: *mut sqlite3_stmt, col: c_int) -> c_int;
    fn sqlite3_changes(db: *mut sqlite3) -> c_int;
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_NULL: c_int = 5;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;

/// SQLITE_TRANSIENT: SQLite copies the bytes before returning.
fn transient() -> Option<unsafe extern "C" fn(*mut c_void)> {
    unsafe { std::mem::transmute::<isize, Option<unsafe extern "C" fn(*mut c_void)>>(-1) }
}

/// One bound parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_string())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}
impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Blob(v)
    }
}
impl<T> From<Option<T>> for Value
where
    T: Into<Value>,
{
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(Value::Null)
    }
}

/// One result column.
#[derive(Debug, Clone, PartialEq)]
pub enum Column {
    Null,
    Int(i64),
    Text(String),
    Blob(Vec<u8>),
}

impl Column {
    pub fn as_i64(&self) -> i64 {
        match self {
            Column::Int(v) => *v,
            _ => 0,
        }
    }
    pub fn as_str(&self) -> &str {
        match self {
            Column::Text(v) => v,
            _ => "",
        }
    }
    pub fn as_blob(&self) -> &[u8] {
        match self {
            Column::Blob(v) => v,
            _ => &[],
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Column::Null)
    }
}

/// A single result row.
#[derive(Debug, Clone)]
pub struct Row(pub Vec<Column>);

impl Row {
    pub fn get(&self, i: usize) -> &Column {
        &self.0[i]
    }
    pub fn int(&self, i: usize) -> i64 {
        self.0[i].as_i64()
    }
    pub fn text(&self, i: usize) -> String {
        self.0[i].as_str().to_string()
    }
    pub fn blob(&self, i: usize) -> Vec<u8> {
        self.0[i].as_blob().to_vec()
    }
    pub fn opt_text(&self, i: usize) -> Option<String> {
        match &self.0[i] {
            Column::Text(v) => Some(v.clone()),
            _ => None,
        }
    }
}

/// An open SQLite connection. Go uses a pool; Rust serializes with a Mutex.
pub struct Conn {
    raw: *mut sqlite3,
}

unsafe impl Send for Conn {}

impl Conn {
    pub fn open(path: &str) -> Result<Conn> {
        let c = CString::new(path).map_err(|e| Error::msg(e.to_string()))?;
        let mut raw: *mut sqlite3 = ptr::null_mut();
        let rc = unsafe {
            sqlite3_open_v2(
                c.as_ptr(),
                &mut raw,
                SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
                ptr::null(),
            )
        };
        if rc != SQLITE_OK {
            let msg = if raw.is_null() {
                "sqlite open failed".to_string()
            } else {
                unsafe {
                    CStr::from_ptr(sqlite3_errmsg(raw))
                        .to_string_lossy()
                        .into_owned()
                }
            };
            if !raw.is_null() {
                unsafe {
                    sqlite3_close(raw);
                }
            }
            return Err(Error::msg(format!("sqlite open: {msg}")));
        }
        Ok(Conn { raw })
    }

    pub fn open_memory() -> Result<Conn> {
        Conn::open(":memory:")
    }

    fn errmsg(&self) -> String {
        unsafe {
            CStr::from_ptr(sqlite3_errmsg(self.raw))
                .to_string_lossy()
                .into_owned()
        }
    }

    pub fn exec(&self, sql: &str) -> Result<()> {
        let c = CString::new(sql).map_err(|e| Error::msg(e.to_string()))?;
        let rc =
            unsafe { sqlite3_exec(self.raw, c.as_ptr(), None, ptr::null_mut(), ptr::null_mut()) };
        if rc != SQLITE_OK {
            return Err(Error::msg(format!("sqlite exec: {}", self.errmsg())));
        }
        Ok(())
    }

    fn prepare(&self, sql: &str) -> Result<*mut sqlite3_stmt> {
        let c = CString::new(sql).map_err(|e| Error::msg(e.to_string()))?;
        let mut stmt: *mut sqlite3_stmt = ptr::null_mut();
        let rc =
            unsafe { sqlite3_prepare_v2(self.raw, c.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
        if rc != SQLITE_OK {
            return Err(Error::msg(format!("sqlite prepare: {}", self.errmsg())));
        }
        Ok(stmt)
    }

    fn bind(stmt: *mut sqlite3_stmt, params: &[Value]) -> Result<()> {
        for (i, p) in params.iter().enumerate() {
            let idx = (i + 1) as c_int;
            let rc = unsafe {
                match p {
                    Value::Null => sqlite3_bind_null(stmt, idx),
                    Value::Int(v) => sqlite3_bind_int64(stmt, idx, *v),
                    Value::Text(v) => {
                        let c =
                            CString::new(v.as_bytes()).map_err(|e| Error::msg(e.to_string()))?;
                        sqlite3_bind_text(stmt, idx, c.as_ptr(), v.len() as c_int, transient())
                    }
                    Value::Blob(v) => sqlite3_bind_blob(
                        stmt,
                        idx,
                        v.as_ptr() as *const c_void,
                        v.len() as c_int,
                        transient(),
                    ),
                }
            };
            if rc != SQLITE_OK {
                return Err(Error::msg(format!("sqlite bind {idx}: rc={rc}")));
            }
        }
        Ok(())
    }

    fn col(stmt: *mut sqlite3_stmt, i: c_int) -> Column {
        unsafe {
            match sqlite3_column_type(stmt, i) {
                SQLITE_NULL => Column::Null,
                1 => Column::Int(sqlite3_column_int64(stmt, i)),
                3 => {
                    let p = sqlite3_column_text(stmt, i);
                    let n = sqlite3_column_bytes(stmt, i) as usize;
                    let slice = std::slice::from_raw_parts(p, n);
                    Column::Text(String::from_utf8_lossy(slice).into_owned())
                }
                4 => {
                    let n = sqlite3_column_bytes(stmt, i) as usize;
                    let p = sqlite3_column_blob(stmt, i) as *const u8;
                    let slice = if n == 0 {
                        &[][..]
                    } else {
                        std::slice::from_raw_parts(p, n)
                    };
                    Column::Blob(slice.to_vec())
                }
                _ => Column::Null,
            }
        }
    }

    pub fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let stmt = self.prepare(sql)?;
        Self::bind(stmt, params)?;
        let mut out = Vec::new();
        loop {
            let rc = unsafe { sqlite3_step(stmt) };
            if rc == SQLITE_ROW {
                let n = unsafe { sqlite3_column_count(stmt) };
                let row = Row((0..n).map(|i| Self::col(stmt, i)).collect());
                out.push(row);
            } else if rc == SQLITE_DONE {
                break;
            } else {
                unsafe {
                    sqlite3_finalize(stmt);
                }
                return Err(Error::msg(format!(
                    "sqlite step: rc={rc} {}",
                    self.errmsg()
                )));
            }
        }
        unsafe {
            sqlite3_finalize(stmt);
        }
        Ok(out)
    }

    pub fn execute(&self, sql: &str, params: &[Value]) -> Result<u64> {
        let stmt = self.prepare(sql)?;
        Self::bind(stmt, params)?;
        let rc = unsafe { sqlite3_step(stmt) };
        unsafe {
            sqlite3_finalize(stmt);
        }
        if rc != SQLITE_DONE {
            return Err(Error::msg(format!(
                "sqlite step: rc={rc} {}",
                self.errmsg()
            )));
        }
        Ok(unsafe { sqlite3_changes(self.raw) } as u64)
    }
}

extern "C" {
    fn sqlite3_column_count(stmt: *mut sqlite3_stmt) -> c_int;
}

impl Drop for Conn {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                sqlite3_close(self.raw);
            }
        }
    }
}
