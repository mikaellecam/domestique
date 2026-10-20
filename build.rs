//! Gives the Windows executables their icon and name.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            // Task Manager shows these, not the exe's name.
            .set("FileDescription", "Domestique")
            .set("ProductName", "Domestique")
            .compile()
            .expect("can't embed the icon");
    }
}
