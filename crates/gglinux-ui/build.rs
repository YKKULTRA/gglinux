// SPDX-License-Identifier: GPL-3.0-or-later
fn main() {
    let style = std::env::var("GG_STYLE").unwrap_or_else(|_| "fluent-dark".into());
    println!("cargo:rerun-if-env-changed=GG_STYLE");
    let config = slint_build::CompilerConfiguration::new().with_style(style);
    slint_build::compile_with_config("ui/app.slint", config).expect("the UI compiles");
}
