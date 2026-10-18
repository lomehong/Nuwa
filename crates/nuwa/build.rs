//! Release guard for the embedded web console.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=../../web/dist");

    // rust-embed would otherwise silently embed the placeholder, and cargo
    // caching can reuse the stale release binary; fail loudly instead.
    let index = Path::new("../../web/dist/index.html");
    if std::env::var("PROFILE").as_deref() == Ok("release") && !index.is_file() {
        panic!(
            "web/dist/index.html is missing: run `cd web && npm ci && npm run build` \
             before a release build of nuwa"
        );
    }
}
