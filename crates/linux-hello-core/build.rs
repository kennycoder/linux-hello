fn main() {
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=src/face_engine.h");
    println!("cargo:rerun-if-changed=src/face_engine.cpp");

    let mut builder = cxx_build::bridge("src/ffi.rs");
    builder
        .file("src/face_engine.cpp")
        .include("src")
        .std("c++17");

    // Discover OpenCV include directories & link flags:
    // Check pkg-config opencv4, opencv5, or opencv first.
    // probe_library automatically emits cargo:rustc-link-lib and cargo:rustc-link-search
    // matching the exact installed modules on the system.
    let mut include_dirs = Vec::new();
    let pkg_config_success = if let Ok(lib) = pkg_config::probe_library("opencv4")
        .or_else(|_| pkg_config::probe_library("opencv5"))
        .or_else(|_| pkg_config::probe_library("opencv"))
    {
        for inc in lib.include_paths {
            include_dirs.push(inc);
        }
        true
    } else {
        false
    };

    if include_dirs.is_empty() {
        for path in &[
            "/usr/include/opencv4",
            "/usr/include/opencv5",
            "/usr/local/include/opencv4",
            "/usr/local/include/opencv5",
            "/usr/include",
        ] {
            let p = std::path::Path::new(path);
            if p.exists() {
                include_dirs.push(p.to_path_buf());
            }
        }
    }

    for dir in include_dirs {
        builder.include(dir);
    }

    builder.compile("face_engine");

    println!("cargo:rustc-link-lib=static=face_engine");

    // OpenCV libraries to link.
    // If pkg-config successfully resolved the library, it already emitted the exact
    // cargo:rustc-link-lib and cargo:rustc-link-search directives for all installed modules.
    if !pkg_config_success {
        println!("cargo:rustc-link-lib=opencv_core");
        println!("cargo:rustc-link-lib=opencv_dnn");
        println!("cargo:rustc-link-lib=opencv_objdetect");
        println!("cargo:rustc-link-lib=opencv_imgproc");
        println!("cargo:rustc-link-lib=opencv_flann");
    }
}

