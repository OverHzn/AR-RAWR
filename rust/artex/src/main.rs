use artex::config;

#[tokio::main]
async fn main() {
    // Placeholder async entrypoint. The full ARTEX server (config → db → agent →
    // server) lands here incrementally; for now report the resolved config path.
    eprintln!(
        "artex (Rust rewrite) — work in progress; config path: {}",
        config::path()
    );
}
