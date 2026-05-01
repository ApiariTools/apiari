fn main() {
    let dist = std::path::Path::new("../../web/dist");
    if !dist.exists() {
        if let Err(e) = std::fs::create_dir_all(dist) {
            println!("cargo::warning=Failed to create web/dist/: {e}");
            return;
        }
        if let Err(e) = std::fs::write(
            dist.join("index.html"),
            "<!DOCTYPE html><html><body><p>Frontend not built. Run: cd web &amp;&amp; npm install &amp;&amp; npm run build</p></body></html>",
        ) {
            println!("cargo::warning=Failed to write placeholder index.html: {e}");
        }
    }
    println!("cargo::rerun-if-changed=../../web/dist/");
}
