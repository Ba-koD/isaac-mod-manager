fn main() {
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon/app.ico");
        resource.set("ProductName", "Isaac Mod Manager");
        resource.set("FileDescription", "Isaac Mod Manager");
        resource
            .compile()
            .expect("failed to compile Windows application resources");
    }
}
