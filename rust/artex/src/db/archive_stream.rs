//! Archive row streaming — pure port of `db/task_archives.go` helpers.
//!
//! Large LLM histories can exceed PostgreSQL's 256 MiB jsonb array limit, so
//! rows are read in order and assembled as a JSON array (or a newline-delimited
//! JSON sequence) in the client instead of aggregating in the database.

use std::io::Write;

use crate::db::error::{Error, Result};
use crate::db::DB;

/// `encodeArchiveRows`: assemble a JSON array from row-JSON bytes in order.
pub fn encode_archive_rows<I, E>(rows: I) -> std::result::Result<(Vec<u8>, i64), E>
where
    I: IntoIterator<Item = std::result::Result<Vec<u8>, E>>,
{
    let mut out = Vec::new();
    out.push(b'[');
    let mut count = 0i64;
    for raw in rows {
        let raw = raw?;
        if count > 0 {
            out.push(b',');
        }
        out.extend_from_slice(&raw);
        count += 1;
    }
    out.push(b']');
    Ok((out, count))
}

/// `writeArchiveRows`: write each row-JSON as one line of a JSON sequence.
pub fn write_archive_rows<I, E, W: Write>(rows: I, writer: &mut W) -> std::result::Result<i64, E>
where
    I: IntoIterator<Item = std::result::Result<Vec<u8>, E>>,
    E: From<std::io::Error>,
{
    let mut count = 0i64;
    for raw in rows {
        let raw = raw?;
        writer.write_all(&raw).map_err(E::from)?;
        writer.write_all(b"\n").map_err(E::from)?;
        count += 1;
    }
    Ok(count)
}

impl DB {
    /// `queryArchiveRows`: read `inner` as streamed row JSON. `inner` is a
    /// caller-owned SQL fragment (table name / filter), never user input.
    pub async fn query_archive_rows(
        &self,
        inner: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<(Vec<u8>, i64)> {
        let sql = format!("SELECT row_to_json(row_data)::text FROM ({inner}) row_data");
        let rows = self.query(&sql, params).await?;
        let items: Vec<Result<Vec<u8>>> = rows
            .iter()
            .map(|r| Ok(r.get::<_, String>(0).into_bytes()))
            .collect();
        encode_archive_rows(items)
    }

    /// `streamArchiveRows`: newline-delimited variant.
    pub async fn stream_archive_rows<W: Write>(
        &self,
        writer: &mut W,
        inner: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<i64> {
        let sql = format!("SELECT row_to_json(row_data)::text FROM ({inner}) row_data");
        let rows = self.query(&sql, params).await?;
        let items: Vec<Result<Vec<u8>>> = rows
            .iter()
            .map(|r| Ok(r.get::<_, String>(0).into_bytes()))
            .collect();
        write_archive_rows::<_, Error, _>(items, writer)
    }
}
