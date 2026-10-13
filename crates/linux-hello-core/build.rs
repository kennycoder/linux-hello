fn main() {
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=src/face_engine.h");
    println!("cargo:rerun-if-changed=src/face_engine.cpp");

    cxx_build::bridge("src/ffi.rs")
        .file("src/face_engine.cpp")
        .include("src")
        .include("/usr/include/opencv4")
        .std("c++17")
        .compile("face_engine");

    println!("cargo:rustc-link-lib=opencv_core");
    println!("cargo:rustc-link-lib=opencv_dnn");
    println!("cargo:rustc-link-lib=opencv_objdetect");
    println!("cargo:rustc-link-lib=opencv_imgproc");
}

