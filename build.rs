fn main() {
    println!("cargo:rerun-if-changed=assets/icon/app.ico");

    #[cfg(windows)]
    {
        let out_dir = std::path::PathBuf::from(
            std::env::var_os("OUT_DIR").expect("OUT_DIR must be set by Cargo"),
        );
        let icon_path = out_dir.join("app.ico");
        std::fs::write(&icon_path, include_bytes!("assets/icon/app.ico"))
            .expect("failed to write embedded Windows icon");
        let icon_path = icon_path
            .to_str()
            .expect("embedded Windows icon path must be valid UTF-8")
            .to_string();

        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(&icon_path);
        resource.set("ProductName", "Isaac Mod Manager");
        resource.set("FileDescription", "Isaac Mod Manager");
        resource
            .compile()
            .expect("failed to compile Windows application resources");
    }
}
