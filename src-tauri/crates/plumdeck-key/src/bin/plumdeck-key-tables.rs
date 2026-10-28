//! Rewrite contracts/key/keys.json and src/lib/keys.generated.ts.

use std::path::PathBuf;

use plumdeck_key::tables::{keys_json_text, keys_ts_text, JSON_PATH, TS_PATH};

fn main() -> std::io::Result<()> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."));
    for (path, text) in [(JSON_PATH, keys_json_text()), (TS_PATH, keys_ts_text())] {
        let file = root.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, text)?;
        println!("wrote {path}");
    }
    Ok(())
}
