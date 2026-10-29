fn main() {
    slint_build::compile("ui/app.slint").expect("slint build");

    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("icon/icon.ico");
        res.set("FileDescription", "Asset Browser");
        res.set("ProductName", "Asset Browser");
        res.set("OriginalFilename", "asset-library-desktop.exe");
        res.compile().expect("windows icon resource");
    }
}
