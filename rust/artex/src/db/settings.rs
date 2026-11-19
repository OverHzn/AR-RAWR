//! Runtime settings key-value store — port of `db/settings.go`.
//!
//! A tiny key/value table for global app config the UI toggles at runtime
//! (e.g. `traffic_capture`). Missing keys fall back to caller defaults. A
//! *secret* setting (one of [`SECRET_SETTING_KEYS`](crate::db::secrets)) is
//! sealed before it is written and decrypted on read; every other setting is
//! stored in clear.
//!
//! `auth.password_hash`-style keys ("may only be set on first run") rest the
//! guarantee on the `settings` primary key: [`DB::insert_setting_if_absent`]
//! writes with `ON CONFLICT DO NOTHING`, so a concurrent writer or a failed
//! upper-layer check can never silently overwrite the existing value.

use crate::db::error::{Error, Result};
use crate::db::secrets::{open_secret, seal_secret, secret_setting};
use crate::db::DB;

/// `sealSetting`: encrypt the value of a secret setting; other values pass as is.
pub(crate) fn seal_setting(key: &str, value: &str) -> Result<String> {
    if let Some(c) = secret_setting(key) {
        return seal_secret(&c, value).map_err(Error::msg);
    }
    Ok(value.to_string())
}

impl DB {
    /// `GetSetting`: return the stored value and `ok=false` when the key is
    /// unset. A secret setting is decrypted.
    pub async fn get_setting(&self, key: &str) -> Result<(String, bool)> {
        let conn = self.pool.get().await?;
        let row = conn
            .query_opt("SELECT value FROM settings WHERE key=$1", &[&key])
            .await?;
        let Some(row) = row else {
            return Ok((String::new(), false));
        };
        let mut value: String = row.get(0);
        if let Some(c) = secret_setting(key) {
            value = open_secret(&c, &value).map_err(Error::msg)?;
        }
        Ok((value, true))
    }

    /// `SetSetting`: upsert a setting value (sealed first when it is a secret).
    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let value = seal_setting(key, value)?;
        let conn = self.pool.get().await?;
        conn.execute(
            "INSERT INTO settings(key, value) VALUES ($1, $2)
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
            &[&key, &value],
        )
        .await?;
        Ok(())
    }

    /// `InsertSettingIfAbsent`: write only when the key does not exist yet. An
    /// existing key is kept as-is and `false` is returned. The judgment rests on
    /// the primary-key constraint, so a concurrent writer or a failed upper-layer
    /// `GetSetting` check can never overwrite the existing value.
    pub async fn insert_setting_if_absent(&self, key: &str, value: &str) -> Result<bool> {
        let value = seal_setting(key, value)?;
        let conn = self.pool.get().await?;
        let n = conn
            .execute(
                "INSERT INTO settings(key, value) VALUES ($1, $2)
                 ON CONFLICT (key) DO NOTHING",
                &[&key, &value],
            )
            .await?;
        Ok(n > 0)
    }

    /// `RotatePassword`: store `hash` and increment the monotonic `version` in one
    /// transaction. A failure (e.g. a non-numeric version) leaves both rows
    /// unchanged. The returned version is the value new sessions must carry.
    pub async fn rotate_password(
        &self,
        hash_key: &str,
        hash: &str,
        version_key: &str,
    ) -> Result<i64> {
        let mut conn = self.pool.get().await?;
        let tx = conn.transaction().await?;
        let sealed = seal_setting(hash_key, hash)?;
        tx.execute(
            "INSERT INTO settings(key, value) VALUES ($1, $2)
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
            &[&hash_key, &sealed],
        )
        .await?;
        let row = tx
            .query_one(
                "INSERT INTO settings(key, value) VALUES ($1, '1')
                 ON CONFLICT (key) DO UPDATE
                 SET value = (settings.value::bigint + 1)::text, updated_at = now()
                 RETURNING value",
                &[&version_key],
            )
            .await?;
        let raw: String = row.get(0);
        let n: i64 = raw
            .parse()
            .map_err(|e: std::num::ParseIntError| Error::msg(e.to_string()))?;
        tx.commit().await?;
        Ok(n)
    }

    /// `InsertPassword`: write `hash` only when that key is absent, and set the
    /// version to 1 in the same transaction. `inserted=false` when the hash
    /// already exists; the version row is then left as it was.
    pub async fn insert_password(
        &self,
        hash_key: &str,
        hash: &str,
        version_key: &str,
    ) -> Result<(i64, bool)> {
        let mut conn = self.pool.get().await?;
        let tx = conn.transaction().await?;
        let sealed = seal_setting(hash_key, hash)?;
        let n = tx
            .execute(
                "INSERT INTO settings(key, value) VALUES ($1, $2)
                 ON CONFLICT (key) DO NOTHING",
                &[&hash_key, &sealed],
            )
            .await?;
        if n == 0 {
            // The hash already exists; leave the version untouched. Dropping the
            // transaction rolls back (nothing was written).
            return Ok((0, false));
        }
        tx.execute(
            "INSERT INTO settings(key, value) VALUES ($1, '1')
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
            &[&version_key],
        )
        .await?;
        tx.commit().await?;
        Ok((1, true))
    }

    /// `GetBool`: return the boolean setting, or `def` when unset/unparseable.
    pub async fn get_bool(&self, key: &str, def: bool) -> bool {
        match self.get_setting(key).await {
            Ok((v, true)) => v == "true" || v == "1",
            _ => def,
        }
    }

    /// `SetBool`: store a boolean setting as `"true"`/`"false"`.
    pub async fn set_bool(&self, key: &str, val: bool) -> Result<()> {
        if val {
            self.set_setting(key, "true").await
        } else {
            self.set_setting(key, "false").await
        }
    }
}
