fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("编译 Slint 界面失败");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::manifest::ExecutionLevel;
        // 以调用者权限启动，避免每次冷启动都弹 UAC；需要时由用户主动提权。
        embed_manifest::embed_manifest(
            embed_manifest::new_manifest("TaskManagerLite").requested_execution_level(ExecutionLevel::AsInvoker),
        )
        .expect("嵌入 manifest 失败");
    }
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/app.ico");
        res.compile().expect("嵌入图标失败");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app.ico");
}
