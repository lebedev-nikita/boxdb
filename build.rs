use std::path::{Path, PathBuf};

fn c_sources(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory).expect("extension source directory") {
        let path = entry.expect("extension source entry").path();
        if path.is_dir() {
            files.extend(c_sources(&path));
        } else if path.extension().is_some_and(|extension| extension == "c") {
            files.push(path);
        }
    }
    files.sort();
    files
}

fn main() {
    println!("cargo:rerun-if-changed=vendor");
    cc::Build::new()
        .files(c_sources(Path::new("vendor/sqlean/text")))
        .files(c_sources(Path::new("vendor/sqlean/regexp")))
        .include("vendor/sqlean")
        .include(std::env::var("DEP_SQLITE3_INCLUDE").expect("bundled SQLite headers"))
        .define("SQLITE_CORE", None)
        .define("PCRE2_CODE_UNIT_WIDTH", "8")
        .define("LINK_SIZE", "2")
        .define("HAVE_CONFIG_H", None)
        .define("SUPPORT_UNICODE", None)
        .define("PCRE2_STATIC", None)
        .warnings(false)
        .compile("boxdb_extensions");
}
