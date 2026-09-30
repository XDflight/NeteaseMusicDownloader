fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../packaging/icons/app.ico");
    println!("cargo:rerun-if-changed=../../packaging/windows/app.manifest");

    #[cfg(windows)]
    {
        let ico = std::path::Path::new("../../packaging/icons/app.ico");
        let mut res = winresource::WindowsResource::new();
        if ico.exists() {
            res.set_icon(ico.to_str().unwrap());
        }
        res.set("ProductName", "Netease Music Downloader");
        res.set("FileDescription", "Netease Music Downloader");
        res.set("LegalCopyright", "AGPL-3.0-only");
        res.set("OriginalFilename", "netease-music-downloader.exe");
        let manifest = std::path::Path::new("../../packaging/windows/app.manifest");
        if manifest.exists() {
            res.set_manifest_file(manifest.to_str().unwrap());
        }
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}
