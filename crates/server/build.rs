//! rust-embed needs `web/dist` to exist. Create a placeholder page when the UI has not been built,
//! so `cargo build` / `cargo test` work without node.
use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    let index = dist.join("index.html");
    if !index.exists() {
        std::fs::create_dir_all(&dist).expect("create web/dist");
        std::fs::write(
            &index,
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>lineup</title></head>\
             <body><p>lineup: the web UI has not been built. Run <code>pnpm -C web build</code>.</p></body></html>\n",
        )
        .expect("write placeholder index.html");
    }
    // Re-embed when the built UI changes (cargo scans the directory recursively).
    println!("cargo:rerun-if-changed=../../web/dist");
    println!("cargo:rerun-if-changed=build.rs");
}
