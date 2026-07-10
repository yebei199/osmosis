fn main() {
    let config = slint_build::CompilerConfiguration::new()
        // Material 风格在 Android 上缩放效果最好,且支持深色主题。
        // 注意:style 是编译期选定的,六端目前共用这一种。
        .with_style("material".into());
    slint_build::compile_with_config(
        "slint/app.slint",
        config,
    )
    .expect("slint compilation failed");
}
