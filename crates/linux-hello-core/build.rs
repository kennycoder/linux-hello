fn main() {
    println!("cargo:rerun-if-changed=src/face_engine.cpp");

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .include("/usr/include/opencv4")
        .file("src/face_engine.cpp")
        .compile("face_engine");

    println!("cargo:rustc-link-lib=opencv_core");
    println!("cargo:rustc-link-lib=opencv_dnn");
    println!("cargo:rustc-link-lib=opencv_objdetect");
    println!("cargo:rustc-link-lib=opencv_imgproc");
}
