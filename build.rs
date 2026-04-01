fn main() {
    // Recompile when embedded files change (used by include_str! in main.rs).
    println!("cargo:rerun-if-changed=loom.toml");
    println!("cargo:rerun-if-changed=loom.typ.embedded");
    println!("cargo:rerun-if-changed=julia.typ.embedded");
    println!("cargo:rerun-if-changed=r.typ.embedded");
}
