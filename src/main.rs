//! Desktop development entry point.
//! Build with: `cargo run --features desktop`

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("info"),
    )
    .init();
    slint_study::run_app();
}
