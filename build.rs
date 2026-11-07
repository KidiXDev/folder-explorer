fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("failed to compile the Slint UI");

    println!("cargo:rerun-if-changed=assets/app.ico");
    winresource::WindowsResource::new()
        .set_icon("assets/app.ico")
        .compile()
        .expect("failed to embed the executable resources");
}
