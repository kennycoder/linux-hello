#include "face_engine.h"
#include "linux-hello-core/src/ffi.rs.h"

#include <opencv2/core.hpp>
#include <opencv2/core/version.hpp>
#include <opencv2/imgproc.hpp>
#include <opencv2/objdetect.hpp>

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

struct FaceEngineCpp::Impl {
    cv::Ptr<cv::FaceDetectorYN> detector;
    cv::Ptr<cv::FaceRecognizerSF> recognizer;
    int input_w;
    int input_h;
    mutable std::mutex mutex;
};

FaceEngineCpp::FaceEngineCpp(std::unique_ptr<Impl> impl) : impl(std::move(impl)) {}
FaceEngineCpp::~FaceEngineCpp() = default;

static bool test_detector_works(cv::Ptr<cv::FaceDetectorYN>& detector, int width, int height) {
    if (!detector) return false;
    try {
        cv::Mat probe(height, width, CV_8UC3, cv::Scalar(128, 128, 128));
        cv::Mat faces;
        detector->setInputSize(cv::Size(width, height));
        detector->detect(probe, faces);
        return true;
    } catch (...) {
        return false;
    }
}

static cv::Ptr<cv::FaceDetectorYN> create_working_detector(const std::string& primary_path, int width, int height) {
    std::vector<std::string> candidates;

    std::string dir;
    size_t last_slash = primary_path.find_last_of("/\\");
    if (last_slash != std::string::npos) {
        dir = primary_path.substr(0, last_slash + 1);
    }

#if CV_VERSION_MAJOR == 4 && CV_VERSION_MINOR < 8
    // OpenCV < 4.8 (e.g. 4.6.0 on Ubuntu 22.04/24.04 and Debian 12) requires 2022mar model format ("loc", "conf", "iou")
    candidates.push_back(dir + "face_detection_yunet_2022mar.onnx");
    candidates.push_back(primary_path);
#else
    // OpenCV 4.8+ supports 2023mar model format ("cls_8", "bbox_8", etc.)
    candidates.push_back(primary_path);
    candidates.push_back(dir + "face_detection_yunet_2022mar.onnx");
#endif

    for (const auto& path : candidates) {
        if (path.empty()) continue;
        FILE* f = fopen(path.c_str(), "rb");
        if (!f) continue;
        fclose(f);

        try {
            cv::Ptr<cv::FaceDetectorYN> det = cv::FaceDetectorYN::create(
                path, "", cv::Size(width, height), 0.6f, 0.3f, 5000
            );
            if (det && test_detector_works(det, width, height)) {
                return det;
            }
        } catch (...) {
            continue;
        }
    }
    return nullptr;
}

std::unique_ptr<FaceEngineCpp> new_face_engine(
    rust::Str det_model_path,
    rust::Str rec_model_path,
    int32_t width,
    int32_t height
) {
    std::string det_path(det_model_path.data(), det_model_path.size());
    std::string rec_path(rec_model_path.data(), rec_model_path.size());

    auto impl = std::make_unique<FaceEngineCpp::Impl>();
    impl->input_w = width;
    impl->input_h = height;
    impl->detector = create_working_detector(det_path, width, height);
    impl->recognizer = cv::FaceRecognizerSF::create(rec_path, "");

    if (!impl->detector || !impl->recognizer) {
        throw std::runtime_error("Failed to initialize YuNet detector or SFace recognizer");
    }

    return std::make_unique<FaceEngineCpp>(std::move(impl));
}

rust::Vec<RawFace> detect_and_embed(
    const FaceEngineCpp& engine,
    rust::Slice<const uint8_t> gray_data,
    int32_t width,
    int32_t height,
    size_t max_faces
) {
    if (!engine.impl || !engine.impl->detector || !engine.impl->recognizer) {
        throw std::runtime_error("FaceEngine is not initialized");
    }
    if (width <= 0 || height <= 0 || gray_data.size() < static_cast<size_t>(width) * static_cast<size_t>(height)) {
        throw std::runtime_error("Invalid frame dimensions or buffer size");
    }

    std::lock_guard<std::mutex> lock(engine.impl->mutex);

    if (width != engine.impl->input_w || height != engine.impl->input_h) {
        engine.impl->detector->setInputSize(cv::Size(width, height));
        engine.impl->input_w = width;
        engine.impl->input_h = height;
    }

    // Wrap raw 8-bit grayscale buffer
    cv::Mat gray(height, width, CV_8UC1, const_cast<uint8_t*>(gray_data.data()));
    cv::Mat bgr;
    cv::cvtColor(gray, bgr, cv::COLOR_GRAY2BGR);

    cv::Mat faces;
    engine.impl->detector->detect(bgr, faces);

    // If no face or low confidence (common at larger distance where IR illumination falls off as 1/r^2
    // or when backlit/overexposed), retry detection with CLAHE local contrast enhancement.
    bool used_clahe = false;
    cv::Mat bgr_clahe;
    float best_raw_score = (!faces.empty() && faces.rows > 0) ? faces.at<float>(0, 14) : 0.0f;
    if (faces.empty() || faces.rows == 0 || best_raw_score < 0.78f) {
        cv::Ptr<cv::CLAHE> clahe = cv::createCLAHE(2.5, cv::Size(8, 8));
        cv::Mat gray_clahe;
        clahe->apply(gray, gray_clahe);
        cv::cvtColor(gray_clahe, bgr_clahe, cv::COLOR_GRAY2BGR);

        cv::Mat faces_clahe;
        engine.impl->detector->detect(bgr_clahe, faces_clahe);
        float best_clahe_score = (!faces_clahe.empty() && faces_clahe.rows > 0) ? faces_clahe.at<float>(0, 14) : 0.0f;
        if (best_clahe_score > best_raw_score + 0.03f) {
            faces = faces_clahe;
            used_clahe = true;
        }
    }

    rust::Vec<RawFace> out_faces;
    if (faces.empty() || faces.rows <= 0) {
        return out_faces;
    }

    size_t count = std::min(static_cast<size_t>(faces.rows), max_faces);
    out_faces.reserve(count);

    for (size_t i = 0; i < count; ++i) {
        int row_idx = static_cast<int>(i);
        RawFace rf{};
        rf.x = faces.at<float>(row_idx, 0);
        rf.y = faces.at<float>(row_idx, 1);
        rf.width = faces.at<float>(row_idx, 2);
        rf.height = faces.at<float>(row_idx, 3);

        for (size_t l = 0; l < 10; ++l) {
            rf.landmarks[l] = faces.at<float>(row_idx, static_cast<int>(4 + l));
        }
        rf.score = faces.at<float>(row_idx, 14);

        // Align and crop to canonical 112x112 face
        cv::Mat aligned_face;
        cv::Mat face_row = faces.row(row_idx);
        engine.impl->recognizer->alignCrop(used_clahe ? bgr_clahe : bgr, face_row, aligned_face);

        // If the aligned face crop has very low contrast or is dim/blown out at distance,
        // normalize local contrast on the crop before feature extraction.
        if (!used_clahe && !aligned_face.empty()) {
            cv::Scalar mean_s, std_s;
            cv::meanStdDev(aligned_face, mean_s, std_s);
            if (mean_s[0] < 48.0 || std_s[0] < 22.0 || mean_s[0] > 205.0) {
                cv::Mat crop_gray;
                cv::cvtColor(aligned_face, crop_gray, cv::COLOR_BGR2GRAY);
                cv::Ptr<cv::CLAHE> crop_clahe = cv::createCLAHE(2.0, cv::Size(4, 4));
                cv::Mat crop_eq;
                crop_clahe->apply(crop_gray, crop_eq);
                cv::cvtColor(crop_eq, aligned_face, cv::COLOR_GRAY2BGR);
            }
        }

        // Extract 128-D feature vector
        cv::Mat feat;
        engine.impl->recognizer->feature(aligned_face, feat);

        if (feat.isContinuous() && feat.total() == 128) {
            std::memcpy(rf.embedding.data(), feat.ptr<float>(), 128 * sizeof(float));
        } else {
            for (size_t d = 0; d < 128; ++d) {
                rf.embedding[d] = feat.at<float>(0, static_cast<int>(d));
            }
        }

        out_faces.push_back(rf);
    }

    return out_faces;
}

