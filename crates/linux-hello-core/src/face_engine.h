#pragma once

#include <cstddef>
#include <cstdint>
#include <memory>
#include "rust/cxx.h"

struct RawFace;

struct FaceEngineCpp {
    struct Impl;
    std::unique_ptr<Impl> impl;

    explicit FaceEngineCpp(std::unique_ptr<Impl> impl);
    ~FaceEngineCpp();
};

std::unique_ptr<FaceEngineCpp> new_face_engine(
    rust::Str det_model_path,
    rust::Str rec_model_path,
    int32_t width,
    int32_t height
);

rust::Vec<RawFace> detect_and_embed(
    const FaceEngineCpp& engine,
    rust::Slice<const uint8_t> gray_data,
    int32_t width,
    int32_t height,
    size_t max_faces
);
