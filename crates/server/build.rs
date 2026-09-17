use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn files(directory: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for entry in fs::read_dir(directory).expect("Read documentation directory") {
        let entry = entry.expect("Read documentation entry");
        let kind = entry.file_type().expect("Inspect documentation entry");
        assert!(
            !kind.is_symlink(),
            "Documentation must not contain symbolic links"
        );
        if kind.is_dir() {
            result.extend(files(&entry.path()));
        } else if kind.is_file() {
            result.push(entry.path());
        }
    }
    result.sort();
    result
}

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    println!(
        "cargo:rerun-if-changed={}",
        root.join("book.toml").display()
    );
    println!("cargo:rerun-if-changed={}", root.join("book/src").display());
    for record in ["AGENTS.md", "HANDOFF.md", "DURABLE-RECORD.md"] {
        println!("cargo:rerun-if-changed={}", root.join(record).display());
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let destination = out.join("documentation");
    if destination.exists() {
        fs::remove_dir_all(&destination).expect("Clear generated documentation");
    }
    // Embedded public content comes only from the checked-in configuration;
    // ambient MDBOOK_* overrides must not select a different source directory.
    let mut config = mdbook_driver::config::Config::from_disk(root.join("book.toml"))
        .expect("Read checked-in book configuration");
    config.build.build_dir = destination.clone();
    let book = mdbook_driver::MDBook::load_with_config(&root, config)
        .expect("Load maintained mdBook without environment overrides");
    book.build()
        .expect("Build embedded mdBook with pinned renderer");

    // mdBook's initialization scripts run synchronously. Serve the same scripts
    // as local assets so documentation does not require inline-script CSP access.
    fs::create_dir_all(destination.join("_inline")).unwrap();
    for path in files(&destination)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
    {
        let html = fs::read_to_string(&path).unwrap();
        let mut remaining = html.as_str();
        let mut rewritten = String::new();
        while let Some(start) = remaining.find("<script") {
            rewritten.push_str(&remaining[..start]);
            remaining = &remaining[start..];
            let tag_end = remaining.find('>').expect("Script opening tag") + 1;
            let end = remaining[tag_end..]
                .find("</script>")
                .expect("Script closing tag")
                + tag_end;
            let tag = &remaining[..tag_end];
            if tag.contains("src=") {
                rewritten.push_str(&remaining[..end + 9]);
            } else {
                let body = &remaining[tag_end..end];
                let name = format!("{}.js", hex::encode(Sha256::digest(body.as_bytes())));
                fs::write(destination.join("_inline").join(&name), body).unwrap();
                rewritten.push_str(&tag[..tag.len() - 1]);
                rewritten.push_str(&format!(" src=\"/documentation/_inline/{name}\"></script>"));
            }
            remaining = &remaining[end + 9..];
        }
        rewritten.push_str(remaining);
        fs::write(path, rewritten).unwrap();
    }
    let mut source = String::from(
        "pub fn embedded(path: &str) -> Option<(&'static str, &'static [u8])> { match path {\n",
    );
    for path in files(&destination) {
        let key = path
            .strip_prefix(&destination)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/");
        let content_type = match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
            "html" => "text/html; charset=utf-8",
            "js" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "json" => "application/json",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "eot" => "application/vnd.ms-fontobject",
            "ico" => "image/x-icon",
            "txt" => "text/plain; charset=utf-8",
            _ => "application/octet-stream",
        };
        source.push_str(&format!(
            "{key:?} => Some(({content_type:?}, include_bytes!({:?}))),\n",
            path.to_str().unwrap()
        ));
    }
    source.push_str("_ => None, } }\n");
    fs::write(out.join("documentation.rs"), source).unwrap();
}
