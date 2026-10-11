use std::os::raw::{c_char, c_float, c_int, c_uchar};

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct RawFace {
    pub x: c_float,
    pub y: c_float,
    pub width: c_float,
    pub height: c_float,
    pub score: c_float,
    pub landmarks: [c_float; 10],
    pub embedding: [c_float; 128],
}

impl Default for RawFace {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            score: 0.0,
            landmarks: [0.0; 10],
            embedding: [0.0; 128],
        }
    }
}

pub enum FaceEngineOpaque {}

extern "C" {
    pub fn face_engine_new(
        det_model_path: *const c_char,
        rec_model_path: *const c_char,
        width: c_int,
        height: c_int,
    ) -> *mut FaceEngineOpaque;

    pub fn face_engine_free(engine: *mut FaceEngineOpaque);

    pub fn face_engine_detect_and_embed(
        engine: *mut FaceEngineOpaque,
        gray_data: *const c_uchar,
        width: c_int,
        height: c_int,
        out_faces: *mut RawFace,
        max_faces: c_int,
    ) -> c_int;

    pub fn face_engine_cosine_distance(
        feat1: *const c_float,
        feat2: *const c_float,
        len: c_int,
    ) -> c_float;
}
