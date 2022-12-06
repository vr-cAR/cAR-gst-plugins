use std::{env, path::PathBuf};

#[cfg(feature = "theta")]
fn build_theta() {
    let dst = cmake::Config::new("libuvc-theta")
        .define("CMAKE_BUILD_TARGET", "Static")
        .define("BUILD_EXAMPLE", "OFF")
        .define("CMAKE_BUILD_TYPE", "Release")
        .build();

    println!(
        "cargo:rustc-link-search=native={}",
        dst.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=uvc");
    println!("cargo:rustc-link-lib=usb-1.0");

    let bindings = bindgen::Builder::default()
        .header(
            dst.join("include/libuvc/libuvc.h")
                .into_os_string()
                .into_string()
                .unwrap(),
        )
        .clang_arg(format!(
            "-I{}",
            dst.join("include").into_os_string().into_string().unwrap()
        ))
        .default_enum_style(bindgen::EnumVariation::Rust {
            non_exhaustive: true,
        })
        .derive_default(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        .generate()
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("theta.rs"))
        .expect("Couldn't write bindings!");
}

#[cfg(feature = "k4a")]
fn build_k4a() {
    println!("cargo:rustc-link-lib=dylib=k4a");

    let bindings = bindgen::Builder::default()
        .header("k4a_wrapper.h")
        .default_enum_style(bindgen::EnumVariation::Rust {
            non_exhaustive: true,
        })
        .derive_default(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        .generate()
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("k4a.rs"))
        .expect("Couldn't write bindings!");
}

fn main() {
    #[cfg(feature = "theta")]
    build_theta();
    #[cfg(feature = "k4a")]
    build_k4a();
}
