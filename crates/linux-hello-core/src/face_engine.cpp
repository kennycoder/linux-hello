#include <opencv2/opencv.hpp>
#include <opencv2/objdetect.hpp>
#include <opencv2/core/version.hpp>
#include <vector>
#include <string>
#include <cstring>
#include <cmath>
#include <cstdio>
#include <algorithm>

extern "C" {

struct RawFace {
    float x;
    float y;
    float width;
    float height;
    float score;
    float landmarks[10]; // 5 points: (x, y) * 5
    float embedding[128];
};

struct FaceEngine {
    cv::Ptr<cv::FaceDetectorYN> detector;
    cv::Ptr<cv::FaceRecognizerSF> recognizer;
    int input_w;
    int input_h;
};

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

static cv::Ptr<cv::FaceDetectorYN> create_working_detector(const char* det_model_path, int width, int height) {
    std::string primary_path = det_model_path ? det_model_path : "";
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

FaceEngine* face_engine_new(const char* det_model_path, const char* rec_model_path, int width, int height) {
    try {
        FaceEngine* engine = new FaceEngine();
        engine->input_w = width;
        engine->input_h = height;
        engine->detector = create_working_detector(det_model_path, width, height);
        engine->recognizer = cv::FaceRecognizerSF::create(rec_model_path, "");
        if (!engine->detector || !engine->recognizer) {
            delete engine;
            return nullptr;
        }
        return engine;
    } catch (const std::exception& e) {
        fprintf(stderr, "[face_engine_new] Exception: %s\n", e.what());
        return nullptr;
    } catch (...) {
        fprintf(stderr, "[face_engine_new] Unknown exception\n");
        return nullptr;
    }
}

void face_engine_free(FaceEngine* engine) {
    if (engine) {
        delete engine;
    }
}

int face_engine_detect_and_embed(
    FaceEngine* engine,
    const uint8_t* gray_data,
    int width,
    int height,
    RawFace* out_faces,
    int max_faces
) {
    if (!engine || !engine->detector || !engine->recognizer || !gray_data || !out_faces || max_faces <= 0) {
        return -1;
    }

    try {
        if (width != engine->input_w || height != engine->input_h) {
            engine->detector->setInputSize(cv::Size(width, height));
            engine->input_w = width;
            engine->input_h = height;
        }

        // Wrap raw 8-bit grayscale buffer
        cv::Mat gray(height, width, CV_8UC1, const_cast<uint8_t*>(gray_data));
        cv::Mat bgr;
        cv::cvtColor(gray, bgr, cv::COLOR_GRAY2BGR);

        cv::Mat faces;
        engine->detector->detect(bgr, faces);

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
            engine->detector->detect(bgr_clahe, faces_clahe);
            float best_clahe_score = (!faces_clahe.empty() && faces_clahe.rows > 0) ? faces_clahe.at<float>(0, 14) : 0.0f;
            if (best_clahe_score > best_raw_score + 0.03f) {
                faces = faces_clahe;
                used_clahe = true;
            }
        }

        if (faces.empty() || faces.rows == 0) {
            return 0;
        }

        int count = std::min(faces.rows, max_faces);
        for (int i = 0; i < count; ++i) {
            RawFace& rf = out_faces[i];
            rf.x = faces.at<float>(i, 0);
            rf.y = faces.at<float>(i, 1);
            rf.width = faces.at<float>(i, 2);
            rf.height = faces.at<float>(i, 3);

            for (int l = 0; l < 10; ++l) {
                rf.landmarks[l] = faces.at<float>(i, 4 + l);
            }
            rf.score = faces.at<float>(i, 14);

            // Align and crop to canonical 112x112 face
            cv::Mat aligned_face;
            cv::Mat face_row = faces.row(i);
            engine->recognizer->alignCrop(used_clahe ? bgr_clahe : bgr, face_row, aligned_face);

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
            engine->recognizer->feature(aligned_face, feat);

            if (feat.isContinuous() && feat.total() == 128) {
                std::memcpy(rf.embedding, feat.ptr<float>(), 128 * sizeof(float));
            } else {
                for (int d = 0; d < 128; ++d) {
                    rf.embedding[d] = feat.at<float>(0, d);
                }
            }
        }
        return count;
    } catch (const cv::Exception& e) {
        fprintf(stderr, "[face_engine_detect_and_embed] OpenCV exception: %s\n", e.what());
        return -1;
    } catch (const std::exception& e) {
        fprintf(stderr, "[face_engine_detect_and_embed] std exception: %s\n", e.what());
        return -1;
    } catch (...) {
        fprintf(stderr, "[face_engine_detect_and_embed] unknown exception\n");
        return -1;
    }
}

float face_engine_cosine_distance(const float* feat1, const float* feat2, int len) {
    if (!feat1 || !feat2 || len <= 0) return 0.0f;
    double dot = 0.0;
    double norm1 = 0.0;
    double norm2 = 0.0;
    for (int i = 0; i < len; ++i) {
        dot += feat1[i] * feat2[i];
        norm1 += feat1[i] * feat1[i];
        norm2 += feat2[i] * feat2[i];
    }
    if (norm1 <= 0.0 || norm2 <= 0.0) return 0.0f;
    return static_cast<float>(dot / (std::sqrt(norm1) * std::sqrt(norm2)));
}

}
