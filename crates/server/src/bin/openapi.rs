//! Prints the OpenAPI document (used to generate the web UI's TypeScript types).
fn main() {
    let doc = lineup_server::http::openapi();
    println!("{}", doc.to_pretty_json().expect("serialize openapi"));
}
