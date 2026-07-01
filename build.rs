//! Build script: compile the vendored MVT protobuf schema to Rust.
//!
//! Uses `prost-build`, which shells out to `protoc` (install via
//! `brew install protobuf` / your package manager). The generated
//! `vector_tile.rs` lands in `OUT_DIR` and is `include!`d by `src/mvt`.

use std::io::Result;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=proto/vector_tile.proto");
    prost_build::compile_protos(&["proto/vector_tile.proto"], &["proto"])?;
    Ok(())
}
