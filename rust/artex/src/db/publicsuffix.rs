//! Vendored `golang.org/x/net/publicsuffix` (v0.62.0) lookup — exact port of the
//! `PublicSuffix` / `EffectiveTLDPlusOne` algorithm over the same embedded PSL
//! data files (copied verbatim from the Go module's `publicsuffix/data/`).
//!
//! This unblocks `RootDomain` (eTLD+1), which `company_scope.go` and the
//! RootDomain-dependent `AssetStore` methods need.

use crate::db::error::Error;

const NODES_BITS_CHILDREN: u64 = 10;
const NODES_BITS_ICANN: u64 = 1;
const NODES_BITS_TEXT_OFFSET: u64 = 16;
const NODES_BITS_TEXT_LENGTH: u64 = 6;
const CHILDREN_BITS_WILDCARD: u32 = 1;
const CHILDREN_BITS_NODE_TYPE: u32 = 2;
const CHILDREN_BITS_HI: u32 = 14;
const CHILDREN_BITS_LO: u32 = 14;

const NODE_TYPE_NORMAL: u32 = 0;
const NODE_TYPE_EXCEPTION: u32 = 1;

const NUM_TLD: u32 = 1450;
const NOT_FOUND: u32 = u32::MAX;

static TEXT: &[u8] = include_bytes!("psl_data/text.bin");
static NODES: &[u8] = include_bytes!("psl_data/nodes.bin");
static CHILDREN: &[u8] = include_bytes!("psl_data/children.bin");

/// `uint40String.get`: 5-byte big-endian node.
fn nodes_get(i: u32) -> u64 {
    let off = i as usize * 5;
    let b = &NODES[off..off + 5];
    (b[4] as u64)
        | ((b[3] as u64) << 8)
        | ((b[2] as u64) << 16)
        | ((b[1] as u64) << 24)
        | ((b[0] as u64) << 32)
}

/// `uint32String.get`: 4-byte big-endian children entry.
fn children_get(i: u32) -> u32 {
    let off = i as usize * 4;
    let b = &CHILDREN[off..off + 4];
    (b[3] as u32) | ((b[2] as u32) << 8) | ((b[1] as u32) << 16) | ((b[0] as u32) << 24)
}

fn node_label(i: u32) -> &'static [u8] {
    let x = nodes_get(i);
    let length = (x & ((1u64 << NODES_BITS_TEXT_LENGTH) - 1)) as usize;
    let x = x >> NODES_BITS_TEXT_LENGTH;
    let offset = (x & ((1u64 << NODES_BITS_TEXT_OFFSET) - 1)) as usize;
    &TEXT[offset..offset + length]
}

/// Binary search for `label` in nodes `[lo, hi)` (byte-wise, matching Go strings).
fn find(label: &[u8], lo: u32, hi: u32) -> u32 {
    let mut lo = lo;
    let mut hi = hi;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        match node_label(mid).cmp(label) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Equal => return mid,
            std::cmp::Ordering::Greater => hi = mid,
        }
    }
    NOT_FOUND
}

/// `PublicSuffix`: returns `(public_suffix, icann)`; a bare IP returns `(domain, false)`.
pub fn public_suffix(domain: &str) -> (String, bool) {
    if domain.parse::<std::net::IpAddr>().is_ok() {
        return (domain.to_string(), false);
    }
    let d = domain.as_bytes();
    let mut lo: u32 = 0;
    let mut hi: u32 = NUM_TLD;
    let mut s_end = d.len(); // current suffix string is d[..s_end]
    let mut suffix = d.len();
    let mut icann = false;
    let mut icann_node = false;
    let mut wildcard = false;

    loop {
        let s = &d[..s_end];
        // last index of '.' in s, byte-wise.
        let dot = s
            .iter()
            .rposition(|&b| b == b'.')
            .map(|i| i as i64)
            .unwrap_or(-1);
        if wildcard {
            icann = icann_node;
            suffix = (1 + dot) as usize;
        }
        if lo == hi {
            break;
        }
        let label = if dot >= 0 {
            &s[(dot + 1) as usize..]
        } else {
            s
        };
        let f = find(label, lo, hi);
        if f == NOT_FOUND {
            break;
        }
        let mut u = nodes_get(f) >> (NODES_BITS_TEXT_OFFSET + NODES_BITS_TEXT_LENGTH);
        icann_node = (u & ((1u64 << NODES_BITS_ICANN) - 1)) != 0;
        u >>= NODES_BITS_ICANN;
        u = children_get((u & ((1u64 << NODES_BITS_CHILDREN) - 1)) as u32) as u64;
        lo = (u as u32) & ((1u32 << CHILDREN_BITS_LO) - 1);
        u >>= CHILDREN_BITS_LO;
        hi = (u as u32) & ((1u32 << CHILDREN_BITS_HI) - 1);
        u >>= CHILDREN_BITS_HI;
        match (u as u32) & ((1u32 << CHILDREN_BITS_NODE_TYPE) - 1) {
            NODE_TYPE_NORMAL => suffix = (1 + dot) as usize,
            NODE_TYPE_EXCEPTION => {
                suffix = 1 + s_end;
                break;
            }
            _ => {}
        }
        u >>= CHILDREN_BITS_NODE_TYPE;
        wildcard = ((u as u32) & ((1u32 << CHILDREN_BITS_WILDCARD) - 1)) != 0;
        if !wildcard {
            icann = icann_node;
        }
        if dot == -1 {
            break;
        }
        s_end = dot as usize;
    }

    if suffix == d.len() {
        let last_dot = d
            .iter()
            .rposition(|&b| b == b'.')
            .map(|i| i + 1)
            .unwrap_or(0);
        (domain[last_dot..].to_string(), icann)
    } else {
        (domain[suffix..].to_string(), icann)
    }
}

/// `EffectiveTLDPlusOne`: the registrable domain (eTLD + one label).
pub fn effective_tld_plus_one(domain: &str) -> std::result::Result<String, Error> {
    if domain.starts_with('.') || domain.ends_with('.') || domain.contains("..") {
        return Err(Error::msg(format!(
            "publicsuffix: empty label in domain {domain:?}"
        )));
    }
    let (suffix, _) = public_suffix(domain);
    if domain.len() <= suffix.len() {
        return Err(Error::msg(format!(
            "publicsuffix: cannot derive eTLD+1 for domain {domain:?}"
        )));
    }
    let i = domain.len() - suffix.len() - 1;
    if domain.as_bytes()[i] != b'.' {
        return Err(Error::msg(format!(
            "publicsuffix: invalid public suffix {suffix:?} for domain {domain:?}"
        )));
    }
    let prefix = &domain[..i];
    let start = prefix.rfind('.').map(|p| p + 1).unwrap_or(0);
    Ok(domain[start..].to_string())
}

/// `RootDomain` (db/nkey.go): the registrable domain of `host`, plus whether the
/// host itself IS that apex. IPs and unclassifiable hosts are returned unchanged
/// with `is_apex = true` (best-effort, never treated as a subdomain).
pub fn root_domain(host: &str) -> (String, bool) {
    let h = db_rs::port::domain_key(host.to_string()).unwrap_or_default();
    if h.is_empty() || h.parse::<std::net::IpAddr>().is_ok() {
        return (h, true);
    }
    match effective_tld_plus_one(&h) {
        Ok(e) => {
            let apex = h == e;
            (e, apex)
        }
        Err(_) => (h, true),
    }
}
