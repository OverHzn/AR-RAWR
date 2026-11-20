//! Company entity layer — port of the company entity / read / attribution methods
//! in `db/companies.go` (`UpsertCompany`, `GetCompany`, `GetCompanyByName`,
//! `UpsertByName`, `DeleteCompany`, `DeleteCompanyWithAssets`, `ListCompanies`,
//! `GetScope`, `RecomputeAttribution`, `MalformedIPAssetWarning`,
//! `ResolveCompany`, `ResolveCompanyWithICP`).
//!
//! The scope *parsing* half (`AddScope` / `UpdateScope` / `CreateCompanyWithScope`
//! and the `company_scope.go` parser, which needs `idna` + `publicsuffix`) is a
//! separate later unit; this module is the pure-SQL company CRUD + reads + the
//! derived-ownership recompute / resolve queries.
//!
//! `companyNKey` / `NormalizeICP` / `MalformedIPAssetWarningText` are reused from
//! the gate-pinned `dbx-rs` / `db-rs` parity crates.

use crate::db::{Error, Result, DB};

/// `companyNKey`: normalise a company name (lower-case + collapse whitespace) —
/// re-export of the gate-pinned `dbx-rs::port` helper.
pub fn company_n_key(name: &str) -> String {
    dbx_rs::port::company_n_key(name)
}

/// `ErrCompanyNameConflict`: the unique-name conflict sentinel (Go
/// `errors.New("company name already exists")`).
pub const ERR_COMPANY_NAME_CONFLICT: &str = "company name already exists";
/// `ErrCompanyNotFound`: the missing-company sentinel (Go
/// `errors.New("company not found")`).
pub const ERR_COMPANY_NOT_FOUND: &str = "company not found";

/// `companyScopeMutationLock`: the advisory-lock key that serialises scope writes
/// (and every derived-attribution recompute) so the committed ownership always
/// reflects the latest committed rules. Shared with task-create company inserts.
pub(crate) const COMPANY_SCOPE_MUTATION_LOCK: i64 = 7_337_741_003;
/// `malformedIPAssetsSampled`: how many offending ids one warning names.
const MALFORMED_IP_ASSETS_SAMPLED: i64 = 5;

/// `Company`: one row in the `companies` table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Company {
    pub id: i64,
    pub name: String,
    pub nkey: String,
    pub logo: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// `ScopeRule`: one row in the `company_scope` table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeRule {
    pub id: i64,
    pub company_id: i64,
    pub kind: String,
    pub domain: String,
    pub net: String,
    pub value: String,
    pub raw: String,
    pub reason: String,
}

/// `CompanyWithScope`: a company plus its scope rules and asset count.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompanyWithScope {
    pub company: Company,
    pub scope: Vec<ScopeRule>,
    pub asset_count: i64,
}

/// `CompanyStore` operates on the `companies` + `company_scope` tables. Cheap to
/// clone (the [`DB`] pool is `Arc`-backed).
#[derive(Clone)]
pub struct CompanyStore {
    db: DB,
}

impl DB {
    /// `Companies`: return the company store.
    pub fn companies(&self) -> CompanyStore {
        CompanyStore { db: self.clone() }
    }
}

/// `logAttributionWarning`: report a recompute warning in the server log. Every
/// recompute path calls it, so the warning surfaces even for triggers with no
/// per-request response (company deletion, scope sync, agent asset writes).
fn log_attribution_warning(warning: &str) {
    if !warning.is_empty() {
        eprintln!("[assets] {warning}");
    }
}

/// Scan rows of the malformed-IP warning query and word the warning (or "" when
/// there are none). Shared by the standalone read and the in-transaction recompute.
fn format_malformed_ip_warning(rows: &[tokio_postgres::Row]) -> String {
    let mut total = 0i64;
    let mut samples: Vec<String> = Vec::new();
    for r in rows {
        let id: i64 = r.get(0);
        let ip: String = r.get(1);
        total = r.get(2);
        samples.push(format!("#{id} {ip}"));
    }
    if total == 0 {
        return String::new();
    }
    dbx_rs::port::malformed_ip_asset_warning_text(total, &samples)
}

const MALFORMED_IP_ASSET_SQL: &str = "
	SELECT id, ip, count(*) OVER () AS total
	FROM assets
	WHERE ip IS NOT NULL AND ip <> '' AND try_inet(ip) IS NULL
	  AND type IN ('ip','subdomain','service','endpoint')
	ORDER BY id
	LIMIT $1";

impl CompanyStore {
    /// `UpsertCompany`: create or update a company by name. Returns the id and
    /// whether a new row was created (`xmax = 0`).
    pub async fn upsert_company(&self, name: &str, logo: &str) -> Result<(i64, bool)> {
        let nkey = company_n_key(name);
        let logo_val: Option<&str> = if logo.is_empty() { None } else { Some(logo) };
        let row = self
            .db
            .query_one(
                "INSERT INTO companies(name, nkey, logo) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (nkey) DO UPDATE SET \
             name = EXCLUDED.name, \
             logo = COALESCE(EXCLUDED.logo, companies.logo), \
             updated_at = now() \
             RETURNING id, (xmax = 0)",
                &[&name, &nkey, &logo_val],
            )
            .await?;
        Ok((row.get(0), row.get(1)))
    }

    /// `GetCompany`: one company by id (None when not found).
    pub async fn get_company(&self, id: i64) -> Result<Option<Company>> {
        let row = self
            .db
            .query_opt(
                "SELECT id, name, nkey, logo, created_at::text, updated_at::text \
             FROM companies WHERE id = $1",
                &[&id],
            )
            .await?;
        Ok(row.map(|r| Company {
            id: r.get(0),
            name: r.get(1),
            nkey: r.get(2),
            logo: r.get(3),
            created_at: r.get(4),
            updated_at: r.get(5),
        }))
    }

    /// `GetCompanyByName`: one company by normalised name (None when not found).
    pub async fn get_company_by_name(&self, name: &str) -> Result<Option<Company>> {
        let nkey = company_n_key(name);
        let row = self
            .db
            .query_opt(
                "SELECT id, name, nkey, logo, created_at::text, updated_at::text \
             FROM companies WHERE nkey = $1",
                &[&nkey],
            )
            .await?;
        Ok(row.map(|r| Company {
            id: r.get(0),
            name: r.get(1),
            nkey: r.get(2),
            logo: r.get(3),
            created_at: r.get(4),
            updated_at: r.get(5),
        }))
    }

    /// `UpsertByName`: create the company if it does not exist, then return its id.
    pub async fn upsert_by_name(&self, name: &str) -> Result<i64> {
        let (id, _) = self.upsert_company(name, "").await?;
        Ok(id)
    }

    /// `DeleteCompany`: delete a company and re-evaluate automatic ownership
    /// against the remaining companies in the same transaction.
    pub async fn delete_company(&self, id: i64) -> Result<()> {
        self.delete_company_with_assets(id, false).await?;
        Ok(())
    }

    /// `DeleteCompanyWithAssets`: delete a company and optionally all of its assets
    /// in one transaction, then re-evaluate ownership against the remaining
    /// companies. Returns the number of assets deleted (0 unless `delete_assets`).
    pub async fn delete_company_with_assets(&self, id: i64, delete_assets: bool) -> Result<i64> {
        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "SELECT pg_advisory_xact_lock($1)",
            &[&COMPANY_SCOPE_MUTATION_LOCK],
        )
        .await?;

        let mut assets_deleted = 0i64;
        if delete_assets {
            assets_deleted = tx
                .execute("DELETE FROM assets WHERE company_id = $1", &[&id])
                .await? as i64;
        }
        let deleted = tx
            .execute("DELETE FROM companies WHERE id = $1", &[&id])
            .await?;
        if deleted == 0 {
            // tx drops here -> rollback; Go returns 0, ErrCompanyNotFound.
            return Err(Error::sentinel(
                ERR_COMPANY_NOT_FOUND,
                ERR_COMPANY_NOT_FOUND,
            ));
        }
        let warning = recompute_attribution_tx(&tx).await?;
        log_attribution_warning(&warning);
        tx.commit().await?;
        Ok(assets_deleted)
    }

    /// `ListCompanies`: all companies with scope and asset count, ordered by name.
    pub async fn list_companies(&self) -> Result<Vec<CompanyWithScope>> {
        let rows = self
            .db
            .query(
                "SELECT c.id, c.name, c.nkey, c.logo, c.created_at::text, c.updated_at::text, \
            COUNT(DISTINCT a.id) AS asset_count \
             FROM companies c \
             LEFT JOIN assets a ON a.company_id = c.id \
             GROUP BY c.id \
             ORDER BY c.name",
                &[],
            )
            .await?;
        let mut out: Vec<CompanyWithScope> = rows
            .iter()
            .map(|r| CompanyWithScope {
                company: Company {
                    id: r.get(0),
                    name: r.get(1),
                    nkey: r.get(2),
                    logo: r.get(3),
                    created_at: r.get(4),
                    updated_at: r.get(5),
                },
                scope: Vec::new(),
                asset_count: r.get(6),
            })
            .collect();
        // Fetch scope rules for each company (matches Go's per-company GetScope).
        for cws in &mut out {
            cws.scope = self.get_scope(cws.company.id).await?;
        }
        Ok(out)
    }

    /// `GetScope`: all scope rules for a company, ordered by id.
    pub async fn get_scope(&self, company_id: i64) -> Result<Vec<ScopeRule>> {
        let rows = self
            .db
            .query(
                "SELECT id, company_id, kind, \
            COALESCE(domain,''), COALESCE(net::text,''), COALESCE(value,''), raw, COALESCE(reason,'') \
             FROM company_scope WHERE company_id = $1 ORDER BY id",
                &[&company_id],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| ScopeRule {
                id: r.get(0),
                company_id: r.get(1),
                kind: r.get(2),
                domain: r.get(3),
                net: r.get(4),
                value: r.get(5),
                raw: r.get(6),
                reason: r.get(7),
            })
            .collect())
    }

    /// `RecomputeAttribution`: rebuild scope-derived ownership in one transaction.
    pub async fn recompute_attribution(&self) -> Result<()> {
        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;
        tx.execute(
            "SELECT pg_advisory_xact_lock($1)",
            &[&COMPANY_SCOPE_MUTATION_LOCK],
        )
        .await?;
        let warning = recompute_attribution_tx(&tx).await?;
        log_attribution_warning(&warning);
        tx.commit().await?;
        Ok(())
    }

    /// `MalformedIPAssetWarning`: report assets with an unparseable `ip` outside of
    /// any mutation, so the API can attach the warning to a scope response.
    pub async fn malformed_ip_asset_warning(&self) -> Result<String> {
        let rows = self
            .db
            .query(MALFORMED_IP_ASSET_SQL, &[&MALFORMED_IP_ASSETS_SAMPLED])
            .await?;
        Ok(format_malformed_ip_warning(&rows))
    }

    /// `ResolveCompany`: the company_id for a given root_domain and/or ip, or None
    /// when no scope rule matches.
    pub async fn resolve_company(&self, root_domain: &str, ip_str: &str) -> Result<Option<i64>> {
        self.resolve_company_with_icp(root_domain, ip_str, "").await
    }

    /// `ResolveCompanyWithICP`: like [`resolve_company`](Self::resolve_company) but
    /// also consulting ICP (only after domain and IP/CIDR fail to match).
    pub async fn resolve_company_with_icp(
        &self,
        root_domain: &str,
        ip_str: &str,
        icp: &str,
    ) -> Result<Option<i64>> {
        if !root_domain.is_empty() {
            let row = self
                .db
                .query_opt(
                    "SELECT company_id FROM company_scope \
             WHERE kind = 'domain' AND domain = $1 \
             ORDER BY length(domain) DESC, company_id LIMIT 1",
                    &[&root_domain],
                )
                .await?;
            if let Some(r) = row {
                return Ok(Some(r.get(0)));
            }
        }
        if !ip_str.is_empty() && ip_str.parse::<std::net::IpAddr>().is_ok() {
            let row = self
                .db
                .query_opt(
                    "SELECT company_id FROM company_scope \
             WHERE kind IN ('ip','cidr') AND net >>= ($1::text)::inet \
             ORDER BY masklen(net) DESC, company_id LIMIT 1",
                    &[&ip_str],
                )
                .await?;
            if let Some(r) = row {
                return Ok(Some(r.get(0)));
            }
        }
        let normalized = db_rs::port::normalize_icp(icp.to_string()).unwrap_or_default();
        if !normalized.is_empty() {
            let row = self
                .db
                .query_opt(
                    "SELECT company_id FROM company_scope \
             WHERE kind = 'icp' AND value = $1 \
             ORDER BY company_id LIMIT 1",
                    &[&normalized],
                )
                .await?;
            if let Some(r) = row {
                return Ok(Some(r.get(0)));
            }
        }
        Ok(None)
    }
}

/// `recomputeAttributionTx`: rebuild scope-derived ownership on the open tx,
/// returning a warning for assets whose `ip` column cannot be parsed. Precedence
/// is domain, then IP/CIDR, then exact normalized ICP; keyword rules never
/// attribute assets. Only `company_source='scope'` rows are cleared first.
async fn recompute_attribution_tx(tx: &tokio_postgres::Transaction<'_>) -> Result<String> {
    tx.execute(
        "UPDATE assets SET company_id = NULL, company_source = 'scope' \
     WHERE company_source = 'scope'",
        &[],
    )
    .await?;

    // Domain-based attribution (root_domain exact match).
    tx.execute(
        "WITH matched AS ( \
        SELECT DISTINCT ON (a.id) a.id AS asset_id, cs.company_id \
        FROM assets a \
        JOIN company_scope cs ON cs.kind = 'domain' AND a.root_domain = cs.domain \
        WHERE a.company_id IS NULL \
          AND a.type IN ('root_domain','subdomain','service','endpoint') \
          AND a.root_domain IS NOT NULL \
        ORDER BY a.id, length(cs.domain) DESC, cs.company_id \
     ) \
     UPDATE assets a \
     SET company_id = matched.company_id, company_source = 'scope' \
     FROM matched \
     WHERE a.id = matched.asset_id",
        &[],
    )
    .await?;

    // IP/CIDR attribution for still-unowned assets.
    tx.execute(
        "WITH matched AS ( \
        SELECT DISTINCT ON (a.id) a.id AS asset_id, cs.company_id \
        FROM assets a \
        JOIN company_scope cs ON cs.kind IN ('ip','cidr') AND cs.net >>= try_inet(a.ip) \
        WHERE a.company_id IS NULL \
          AND a.type IN ('ip','subdomain','service','endpoint') \
          AND a.ip IS NOT NULL \
        ORDER BY a.id, masklen(cs.net) DESC, cs.company_id \
     ) \
     UPDATE assets a \
     SET company_id = matched.company_id, company_source = 'scope' \
     FROM matched \
     WHERE a.id = matched.asset_id",
        &[],
    )
    .await?;

    // Exact normalized ICP attribution after domain/network precedence.
    tx.execute(
        "WITH matched AS ( \
        SELECT DISTINCT ON (a.id) a.id AS asset_id, cs.company_id \
        FROM assets a \
        JOIN company_scope cs ON cs.kind = 'icp' \
          AND ( \
            lower(regexp_replace(COALESCE(a.icp,''), '[[:space:]]+', '', 'g')) = cs.value \
            OR lower(regexp_replace(COALESCE(a.app_icp,''), '[[:space:]]+', '', 'g')) = cs.value \
          ) \
        WHERE a.company_id IS NULL \
          AND (COALESCE(a.icp,'') <> '' OR COALESCE(a.app_icp,'') <> '') \
        ORDER BY a.id, cs.company_id \
     ) \
     UPDATE assets a \
     SET company_id = matched.company_id, company_source = 'scope' \
     FROM matched \
     WHERE a.id = matched.asset_id",
        &[],
    )
    .await?;

    let rows = tx
        .query(MALFORMED_IP_ASSET_SQL, &[&MALFORMED_IP_ASSETS_SAMPLED])
        .await?;
    Ok(format_malformed_ip_warning(&rows))
}

// ---------------------------------------------------------------------------
// Scope writes (D-3b) — need the company-scope parser (company_scope.rs) and the
// vendored publicsuffix table, both now available.

use crate::db::company_scope::{parse_scope_input, ParsedScope, ScopeInput};

/// Max runes in one raw scope line / normalized value (Go MaxCompanyScope*Runes).
pub const MAX_COMPANY_SCOPE_RAW_RUNES: usize = 1024;
pub const MAX_COMPANY_SCOPE_VALUE_RUNES: usize = 1024;

/// `ScopeWriteResult`: counts + validation feedback from a scope write.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScopeWriteResult {
    pub added: i64,
    pub skipped: i64,
    pub invalid: i64,
    pub validation_errors: Vec<String>,
}

/// `ValidateCompanyScopeInputBounds`: request-wide limit before parsing.
fn validate_company_scope_input_bounds(inputs: &[ScopeInput]) -> Result<()> {
    for (i, input) in inputs.iter().enumerate() {
        if input.value.chars().count() > MAX_COMPANY_SCOPE_RAW_RUNES {
            return Err(Error::msg(format!(
                "company scope rule #{} raw value is too long: at most {} characters",
                i + 1,
                MAX_COMPANY_SCOPE_RAW_RUNES
            )));
        }
    }
    Ok(())
}

fn parse_scope_inputs(inputs: &[ScopeInput]) -> (Vec<ParsedScope>, i64, Vec<String>) {
    let mut rules = Vec::with_capacity(inputs.len());
    let mut invalid = 0;
    let mut errs = Vec::new();
    for input in inputs {
        match parse_scope_input(input.clone()) {
            Ok(rule) => rules.push(rule),
            Err(e) => {
                invalid += 1;
                errs.push(format!("{}: {e}", input.value));
            }
        }
    }
    (rules, invalid, errs)
}

fn validate_parsed_scope_bounds(rules: &[ParsedScope]) -> Result<()> {
    for (i, rule) in rules.iter().enumerate() {
        if rule.raw.chars().count() > MAX_COMPANY_SCOPE_RAW_RUNES {
            return Err(Error::msg(format!(
                "company scope rule #{} raw value is too long: at most {} characters",
                i + 1,
                MAX_COMPANY_SCOPE_RAW_RUNES
            )));
        }
        if rule.value.chars().count() > MAX_COMPANY_SCOPE_VALUE_RUNES {
            return Err(Error::msg(format!(
                "company scope rule #{} normalized value is too long: at most {} characters",
                i + 1,
                MAX_COMPANY_SCOPE_VALUE_RUNES
            )));
        }
    }
    Ok(())
}

async fn ensure_company_exists_tx(
    tx: &deadpool_postgres::Transaction<'_>,
    company_id: i64,
) -> Result<()> {
    let row = tx
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM companies WHERE id=$1)",
            &[&company_id],
        )
        .await?;
    if !row.get::<_, bool>(0) {
        return Err(Error::sentinel(
            ERR_COMPANY_NOT_FOUND,
            ERR_COMPANY_NOT_FOUND,
        ));
    }
    Ok(())
}

/// Insert one scope rule; `inserted=false` means a duplicate ignored by ON CONFLICT.
async fn insert_scope_rule_tx(
    tx: &deadpool_postgres::Transaction<'_>,
    company_id: i64,
    rule: &ParsedScope,
    reason: &str,
) -> Result<bool> {
    let n = match rule.kind.as_str() {
        "domain" => {
            tx.execute(
                "INSERT INTO company_scope(company_id, kind, domain, raw, reason) \
                 VALUES ($1, 'domain', $2, $3, $4) \
                 ON CONFLICT ON CONSTRAINT uq_sv2_domain DO NOTHING",
                &[&company_id, &rule.domain, &rule.raw, &reason],
            )
            .await?
        }
        "ip" | "cidr" => {
            tx.execute(
                "INSERT INTO company_scope(company_id, kind, net, raw, reason) \
                 VALUES ($1, $2, ($3::text)::cidr, $4, $5) \
                 ON CONFLICT ON CONSTRAINT uq_sv2_net DO NOTHING",
                &[&company_id, &rule.kind, &rule.net, &rule.raw, &reason],
            )
            .await?
        }
        "icp" | "keyword" => {
            tx.execute(
                "INSERT INTO company_scope(company_id, kind, value, raw, reason) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (company_id, kind, value) WHERE kind IN ('icp','keyword') DO NOTHING",
                &[&company_id, &rule.kind, &rule.value, &rule.raw, &reason],
            )
            .await?
        }
        other => {
            return Err(Error::msg(format!(
                "unsupported company scope kind {other:?}"
            )));
        }
    };
    Ok(n > 0)
}

async fn insert_scope_rules_tx(
    tx: &deadpool_postgres::Transaction<'_>,
    company_id: i64,
    rules: &[ParsedScope],
    reason: &str,
) -> Result<(i64, i64, bool)> {
    let mut added = 0;
    let mut skipped = 0;
    let mut needs_attribution = false;
    for rule in rules {
        if insert_scope_rule_tx(tx, company_id, rule, reason).await? {
            added += 1;
            needs_attribution = needs_attribution || rule.kind != "keyword";
        } else {
            skipped += 1;
        }
    }
    Ok((added, skipped, needs_attribution))
}

impl CompanyStore {
    /// `CreateCompanyWithScope`: create a company + its valid initial rules +
    /// derived attribution atomically. Invalid inputs retain the legacy partial
    /// validation contract (valid rules are still stored).
    pub async fn create_company_with_scope(
        &self,
        name: &str,
        logo: &str,
        inputs: &[ScopeInput],
        reason: &str,
    ) -> Result<(i64, ScopeWriteResult)> {
        validate_company_scope_input_bounds(inputs)?;
        let (rules, invalid, errs) = parse_scope_inputs(inputs);
        let mut out = ScopeWriteResult {
            invalid,
            validation_errors: errs,
            ..ScopeWriteResult::default()
        };
        validate_parsed_scope_bounds(&rules)?;

        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            tx.execute(
                "SELECT pg_advisory_xact_lock($1)",
                &[&COMPANY_SCOPE_MUTATION_LOCK],
            )
            .await?;
            let logo_val: Option<&str> = if logo.is_empty() { None } else { Some(logo) };
            let row = tx
                .query_opt(
                    "INSERT INTO companies(name, nkey, logo) VALUES ($1, $2, $3) \
                     ON CONFLICT (nkey) DO NOTHING RETURNING id",
                    &[&name, &company_n_key(name), &logo_val],
                )
                .await?;
            let Some(row) = row else {
                return Err(Error::sentinel(
                    ERR_COMPANY_NAME_CONFLICT,
                    ERR_COMPANY_NAME_CONFLICT,
                ));
            };
            let id: i64 = row.get(0);
            let (added, skipped, needs_attr) =
                insert_scope_rules_tx(&tx, id, &rules, reason).await?;
            if needs_attr {
                let warning = recompute_attribution_tx(&tx).await?;
                log_attribution_warning(&warning);
            }
            Ok::<_, Error>((id, added, skipped))
        }
        .await;
        match run {
            Ok((id, added, skipped)) => {
                tx.commit().await?;
                out.added = added;
                out.skipped = skipped;
                Ok((id, out))
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `AddScopeInputsChecked`: insert structured scope rules, keeping validation
    /// feedback separate from storage/transaction errors.
    pub async fn add_scope_inputs_checked(
        &self,
        company_id: i64,
        inputs: &[ScopeInput],
        reason: &str,
    ) -> Result<ScopeWriteResult> {
        validate_company_scope_input_bounds(inputs)?;
        let (rules, invalid, errs) = parse_scope_inputs(inputs);
        let mut out = ScopeWriteResult {
            invalid,
            validation_errors: errs,
            ..ScopeWriteResult::default()
        };
        validate_parsed_scope_bounds(&rules)?;

        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            tx.execute(
                "SELECT pg_advisory_xact_lock($1)",
                &[&COMPANY_SCOPE_MUTATION_LOCK],
            )
            .await?;
            ensure_company_exists_tx(&tx, company_id).await?;
            if rules.is_empty() {
                return Ok::<_, Error>((0, 0));
            }
            let (added, skipped, needs_attr) =
                insert_scope_rules_tx(&tx, company_id, &rules, reason).await?;
            if needs_attr {
                let warning = recompute_attribution_tx(&tx).await?;
                log_attribution_warning(&warning);
            }
            Ok((added, skipped))
        }
        .await;
        match run {
            Ok((added, skipped)) => {
                tx.commit().await?;
                out.added = added;
                out.skipped = skipped;
                Ok(out)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `AddScopeInputs`: as checked, but storage errors are folded into the
    /// validation-error list (legacy partial-validation contract).
    pub async fn add_scope_inputs(
        &self,
        company_id: i64,
        inputs: &[ScopeInput],
        reason: &str,
    ) -> ScopeWriteResult {
        match self
            .add_scope_inputs_checked(company_id, inputs, reason)
            .await
        {
            Ok(r) => r,
            Err(e) => ScopeWriteResult {
                validation_errors: vec![e.to_string()],
                ..ScopeWriteResult::default()
            },
        }
    }

    /// `AddScope`: parse and insert raw scope lines for a company.
    pub async fn add_scope(
        &self,
        company_id: i64,
        lines: &[String],
        reason: &str,
    ) -> ScopeWriteResult {
        let inputs: Vec<ScopeInput> = lines
            .iter()
            .map(|line| ScopeInput {
                value: line.clone(),
                ..ScopeInput::default()
            })
            .collect();
        self.add_scope_inputs(company_id, &inputs, reason).await
    }

    /// `UpdateScopeInputsChecked`: replace all rules atomically. Any invalid rule
    /// aborts the whole replacement (existing scope untouched).
    pub async fn update_scope_inputs_checked(
        &self,
        company_id: i64,
        inputs: &[ScopeInput],
        reason: &str,
    ) -> Result<ScopeWriteResult> {
        validate_company_scope_input_bounds(inputs)?;
        let (rules, invalid, errs) = parse_scope_inputs(inputs);
        let mut out = ScopeWriteResult {
            invalid,
            validation_errors: errs,
            ..ScopeWriteResult::default()
        };
        if invalid > 0 {
            return Err(Error::msg(format!(
                "company scope contains {invalid} invalid rules; the existing scope was not overwritten"
            )));
        }
        validate_parsed_scope_bounds(&rules)?;

        let mut client = self.db.pool.get().await?;
        let tx = client.transaction().await?;
        let run = async {
            tx.execute(
                "SELECT pg_advisory_xact_lock($1)",
                &[&COMPANY_SCOPE_MUTATION_LOCK],
            )
            .await?;
            ensure_company_exists_tx(&tx, company_id).await?;
            tx.execute(
                "DELETE FROM company_scope WHERE company_id=$1",
                &[&company_id],
            )
            .await?;
            let (added, _, _) = insert_scope_rules_tx(&tx, company_id, &rules, reason).await?;
            // Rebuild even for an empty replacement: removing rules may detach assets.
            let warning = recompute_attribution_tx(&tx).await?;
            log_attribution_warning(&warning);
            Ok::<_, Error>(added)
        }
        .await;
        match run {
            Ok(added) => {
                tx.commit().await?;
                out.added = added;
                Ok(out)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    /// `UpdateScopeInputs`: as checked, folding storage errors into validation errors.
    pub async fn update_scope_inputs(
        &self,
        company_id: i64,
        inputs: &[ScopeInput],
        reason: &str,
    ) -> ScopeWriteResult {
        match self
            .update_scope_inputs_checked(company_id, inputs, reason)
            .await
        {
            Ok(r) => r,
            Err(e) => ScopeWriteResult {
                validation_errors: vec![e.to_string()],
                ..ScopeWriteResult::default()
            },
        }
    }

    /// `UpdateScope`: replace all rules from raw scope lines.
    pub async fn update_scope(
        &self,
        company_id: i64,
        lines: &[String],
        reason: &str,
    ) -> ScopeWriteResult {
        let inputs: Vec<ScopeInput> = lines
            .iter()
            .map(|line| ScopeInput {
                value: line.clone(),
                ..ScopeInput::default()
            })
            .collect();
        self.update_scope_inputs(company_id, &inputs, reason).await
    }
}
