fn main() {
    // src/share_musics.rs embeds the PlumDeck Lite Worker URL from this variable.
    println!("cargo:rerun-if-env-changed=PLUMDECK_SHARE_MUSICS_URL");
    tauri_build::build()
}
