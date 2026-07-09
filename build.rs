fn main() {
    let config = slint_build::CompilerConfiguration::new()
        // Material 风格在 Android 上缩放效果最好,且支持深色主题。
        .with_style("material".into());
    slint_build::compile_with_config(
        "ui/app.slint",
        config,
    )
    .expect("slint compilation failed");
}
