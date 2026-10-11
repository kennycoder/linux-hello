#include <opencv2/opencv.hpp>
#include <opencv2/objdetect.hpp>
#include <vector>
#include <cstring>
#include <cmath>
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

FaceEngine* face_engine_new(const char* det_model_path, const char* rec_model_path, int width, int height) {
    try {
        FaceEngine* engine = new FaceEngine();
        engine->input_w = width;
        engine->input_h = height;
        engine->detector = cv::FaceDetectorYN::create(
            det_model_path, "", cv::Size(width, height), 0.7f, 0.3f, 5000
        );
        engine->recognizer = cv::FaceRecognizerSF::create(rec_model_path, "");
        if (!engine->detector || !engine->recognizer) {
            delete engine;
            return nullptr;
        }
        return engine;
    } catch (...) {
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
            engine->recognizer->alignCrop(bgr, face_row, aligned_face);

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
    } catch (...) {
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
