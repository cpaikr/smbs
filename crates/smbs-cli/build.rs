//! Records the compilation target for `smbs version`.

fn main() {
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=SMBS_BUILD_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
