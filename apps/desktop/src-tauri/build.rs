fn main() {
    println!("cargo:rerun-if-changed=icons/tray-macos.png");
    println!("cargo:rerun-if-changed=icons/tray-windows.png");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // 使用 Tauri 的静态运行库链接，安装版无需另装 Visual C++ 运行库。
        std::env::set_var("STATIC_VCRUNTIME", "true");
    }
    tauri_build::build()
}
