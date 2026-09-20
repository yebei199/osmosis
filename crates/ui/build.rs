fn main() {
    // debug 档一律带上元素调试信息。它是 ElementHandle / MCP 元素树的**前提**,
    // 而少了它两者都不报错,只是查不到任何元素 —— 看起来像界面写错了。
    // 原先只由 `just desktop-dev` 设的 SLINT_EMIT_DEBUG_INFO 供着,于是裸
    // `cargo run` 与 `cargo test` 都掉进这个坑(见 crates/ui/tests/banner.rs)。
    //
    // release 是否带,由 SLINT_EMIT_DEBUG_INFO 决定(见 #99):release.yml
    // 给 MCP 校验的发行包设它为真值,普通发布件不设,产物里就不嵌元素名。
    println!(
        "cargo:rerun-if-env-changed=SLINT_EMIT_DEBUG_INFO"
    );
    let debug_info = std::env::var("PROFILE")
        .is_ok_and(|profile| profile == "debug")
        || std::env::var("SLINT_EMIT_DEBUG_INFO")
            .is_ok_and(|v| v != "0" && !v.is_empty());

    let config = slint_build::CompilerConfiguration::new()
        // Material 风格在 Android 上缩放效果最好,且支持深色主题。
        // 注意:style 是编译期选定的,六端目前共用这一种。
        .with_style("material".into())
        .with_debug_info(debug_info);
    slint_build::compile_with_config(
        "slint/app.slint",
        config,
    )
    .expect("slint compilation failed");
}
