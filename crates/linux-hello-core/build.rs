fn main() {
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=src/face_engine.h");
    println!("cargo:rerun-if-changed=src/face_engine.cpp");

    let mut builder = cxx_build::bridge("src/ffi.rs");
    builder
        .file("src/face_engine.cpp")
        .include("src")
        .std("c++17");

    // Discover OpenCV include directories:
    // Check pkg-config opencv4 or opencv first, then standard fallback directories
    let mut include_dirs = Vec::new();
    if let Ok(lib) = pkg_config::probe_library("opencv4")
        .or_else(|_| pkg_config::probe_library("opencv5"))
        .or_else(|_| pkg_config::probe_library("opencv"))
    {
        for inc in lib.include_paths {
            include_dirs.push(inc);
        }
    }

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

    // OpenCV libraries to link.
    // In OpenCV 5 (and modular builds), cv::flann symbols (used by cv::FaceRecognizerSF / objdetect / features2d)
    // live in opencv_flann or opencv_features2d / opencv_calib3d.
    println!("cargo:rustc-link-lib=opencv_core");
    println!("cargo:rustc-link-lib=opencv_dnn");
    println!("cargo:rustc-link-lib=opencv_objdetect");
    println!("cargo:rustc-link-lib=opencv_imgproc");
    println!("cargo:rustc-link-lib=opencv_flann");
    println!("cargo:rustc-link-lib=opencv_features2d");
    println!("cargo:rustc-link-lib=opencv_calib3d");
}

