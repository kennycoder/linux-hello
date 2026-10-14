#[cxx::bridge]
pub mod ffi {
    #[derive(Debug, Clone)]
    pub struct RawFace {
        pub x: f32,
        pub y: f32,
        pub width: f32,
        pub height: f32,
        pub score: f32,
        pub landmarks: [f32; 10],
        pub embedding: [f32; 128],
    }

    unsafe extern "C++" {
        include!("face_engine.h");

        type FaceEngineCpp;

        pub fn new_face_engine(
            det_model_path: &str,
            rec_model_path: &str,
            width: i32,
            height: i32,
        ) -> Result<UniquePtr<FaceEngineCpp>>;

        pub fn detect_and_embed(
            engine: &FaceEngineCpp,
            gray_data: &[u8],
            width: i32,
            height: i32,
            max_faces: usize,
        ) -> Result<Vec<RawFace>>;
    }
}

pub use ffi::*;

