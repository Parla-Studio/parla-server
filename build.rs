//! Embeds every course file from content/courses into the binary, so a fresh install can seed
//! its content directory without any extra files.

use std::{env, fs, path::Path};

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("content/courses");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut entries = Vec::new();
    if let Ok(read) = fs::read_dir(&dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                println!("cargo:rerun-if-changed={}", path.display());
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let abs = fs::canonicalize(&path).unwrap();
                entries.push(format!("    ({name:?}, include_str!({:?})),", abs.display().to_string()));
            }
        }
    }
    entries.sort();

    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("bundled_courses.rs");
    let code = format!(
        "/// Course files shipped inside the binary: (file name, JSON text).\npub static BUNDLED_COURSES: &[(&str, &str)] = &[\n{}\n];\n",
        entries.join("\n")
    );
    fs::write(out, code).unwrap();
}
